use mobile_linux_api::MobileLinuxError;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use tokio::task::spawn_blocking;

use super::{
    native, normalize_host_path, parse_availability_reason, parse_native_ok, validate_guest_path,
    IosIshRuntime, IosIshRuntimeConfig, ProcessRegistry, RuntimeState,
};

pub(super) fn same_kernel_config(left: &IosIshRuntimeConfig, right: &IosIshRuntimeConfig) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.workspace_host_path = PathBuf::new();
    right.workspace_host_path = PathBuf::new();
    left.stable_workspace_id.clear();
    right.stable_workspace_id.clear();
    left == right
}

impl IosIshRuntime {
    /// Construct a new runtime without mutating filesystem or native state.
    #[must_use]
    pub fn new(mut config: IosIshRuntimeConfig) -> Result<Self, MobileLinuxError> {
        config.managed_root = normalize_host_path(&config.managed_root, "managed_root")?;
        config.app_sandbox_root =
            normalize_host_path(&config.app_sandbox_root, "app_sandbox_root")?;
        config.workspace_host_path =
            normalize_host_path(&config.workspace_host_path, "workspace_host_path")?;
        for path in config
            .protected_host_roots
            .iter_mut()
            .chain(config.allowed_mount_roots.iter_mut())
        {
            *path = normalize_host_path(path, "mount policy root")?;
        }
        for path in [
            &mut config.rootfs_archive_path,
            &mut config.default_mount_path,
            &mut config.rootfs_patch_path,
        ]
        .into_iter()
        .flatten()
        {
            *path = normalize_host_path(path, "resource path")?;
        }
        for path in &config.allowed_guest_roots {
            validate_guest_path(path, "allowed guest root", true)?;
        }
        if config.stable_workspace_id.is_empty()
            || config.stable_workspace_id.contains('/')
            || config.stable_workspace_id == "."
            || config.stable_workspace_id == ".."
        {
            return Err(MobileLinuxError::InvalidRequest(
                "invalid stable workspace id".into(),
            ));
        }
        static PROCESS_RUNTIME: OnceLock<Mutex<ProcessRegistry>> = OnceLock::new();
        let mut registry = PROCESS_RUNTIME
            .get_or_init(|| Mutex::new(ProcessRegistry::default()))
            .lock()
            .map_err(|_| MobileLinuxError::Io("iSH process registry poisoned".into()))?;
        Self::from_registry(&mut registry, config)
    }
}

impl IosIshRuntime {
    pub(super) fn from_registry(
        registry: &mut ProcessRegistry,
        config: IosIshRuntimeConfig,
    ) -> Result<Self, MobileLinuxError> {
        if let Some(identity) = &registry.identity {
            if !same_kernel_config(identity, &config) {
                return Err(MobileLinuxError::InvalidRequest(
                    "iSH permits one immutable kernel/rootfs configuration per app process".into(),
                ));
            }
        } else {
            registry.identity = Some(config.clone());
        }
        registry.sessions.retain(|state| state.strong_count() > 0);
        for state in &registry.sessions {
            if let Some(state) = state.upgrade() {
                if state.config == config {
                    return Ok(Self { state });
                }
            }
        }
        let mut runtime = Self::new_state(config);
        let state = Arc::get_mut(&mut runtime.state).expect("fresh session");
        state.native_lock = registry.native_lock.clone();
        state.kernel_started = registry.kernel_started.clone();
        registry.sessions.push(Arc::downgrade(&runtime.state));
        Ok(runtime)
    }
}

impl IosIshRuntime {
    pub(super) fn new_state(config: IosIshRuntimeConfig) -> Self {
        let workspace_mount = config.default_workspace_mount();
        let persistent_home_mount = config.persistent_home_mount();
        Self {
            state: Arc::new(RuntimeState {
                config,
                mounts: RwLock::new(vec![workspace_mount, persistent_home_mount]),
                tasks: Mutex::new(HashMap::new()),
                events: Mutex::new(VecDeque::new()),
                next_id: AtomicU64::new(1),
                next_sequence: AtomicU64::new(1),
                booted: AtomicBool::new(false),
                rootfs_status_cache: Mutex::new(None),
                kernel_started: Arc::new(AtomicBool::new(false)),
                closed: AtomicBool::new(false),
                lifecycle: Arc::new(tokio::sync::Mutex::new(())),
                pty: Mutex::new(None),
                raw_stdio: Mutex::new(HashMap::new()),
                native_lock: Arc::new(Mutex::new(())),
                #[cfg(test)]
                test_transport: None,
            }),
        }
    }
}

impl IosIshRuntime {
    pub(super) fn ensure_session_open(&self) -> Result<(), MobileLinuxError> {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(MobileLinuxError::InvalidRequest(
                "runtime is closed; boot the same configuration before executing".into(),
            ));
        }
        Ok(())
    }
}

impl IosIshRuntime {
    pub(super) fn ensure_rootfs_mutable(&self) -> Result<(), MobileLinuxError> {
        if self.state.kernel_started.load(Ordering::Acquire) {
            return Err(MobileLinuxError::RestartRequired("iSH kernel cannot be unloaded; restart the app process before repairing or resetting rootfs".into()));
        }
        if self.state.booted.load(Ordering::Acquire) {
            return Err(MobileLinuxError::InvalidRequest(
                "close runtime before changing rootfs".into(),
            ));
        }
        Ok(())
    }
}

impl IosIshRuntime {
    pub(super) fn native_unavailable_reason(&self) -> Option<String> {
        if native::is_available() {
            return None;
        }
        let native_lock = self.state.native_lock.lock().expect("ios-ish native lock");
        let response = native::availability_json();
        drop(native_lock);
        match response {
            Ok(json) => parse_availability_reason(&json)
                .or_else(|| Some("native iSH runtime reports unavailable".to_string())),
            Err(error) => Some(error),
        }
    }
}

impl IosIshRuntime {
    pub(super) fn ensure_native_available(&self) -> Result<(), MobileLinuxError> {
        match self.native_unavailable_reason() {
            Some(reason) => Err(MobileLinuxError::Unavailable(reason)),
            None => Ok(()),
        }
    }
}

impl IosIshRuntime {
    pub(super) fn native_config_json(&self) -> Result<String, MobileLinuxError> {
        serde_json::to_string(&self.state.config.native_payload())
            .map_err(|error| MobileLinuxError::Io(format!("serialize native config: {error}")))
    }
}

impl IosIshRuntime {
    pub(super) async fn native_boot(&self) -> Result<(), MobileLinuxError> {
        self.ensure_native_available()?;
        let config_json = self.native_config_json()?;
        let native_lock = self.state.native_lock.clone();
        let response = spawn_blocking(move || {
            let _guard = native_lock.lock().expect("ios-ish native lock");
            native::boot_json(&config_json)
        })
        .await
        .map_err(|error| MobileLinuxError::Io(format!("join boot: {error}")))?
        .map_err(MobileLinuxError::Io)?;
        parse_native_ok(&response)?;
        Ok(())
    }
}
