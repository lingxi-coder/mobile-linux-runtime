use mobile_linux_api as platform_api;
#[cfg(any(target_os = "android", target_os = "ios"))]
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use crate::conversion::{
    capability_to_ffi, command_request_to_traits, command_result_to_ffi, event_to_ffi,
    mobile_linux_error_to_ffi, mount_spec_to_traits, process_handle_to_ffi,
    process_handle_to_traits, pty_handle_to_ffi, pty_handle_to_traits, pty_open_request_to_traits,
    raw_handle, status_to_ffi, task_snapshot_to_ffi,
};
use crate::events::{RuntimeEventSink, RuntimeEventSinkBridge};
use crate::types::{
    MobileLinuxApiErrorFfi, MobileLinuxCapabilityFfi, MobileLinuxCommandRequestFfi,
    MobileLinuxCommandResultFfi, MobileLinuxEventFfi, MobileLinuxEventKindFfi,
    MobileLinuxMountSpecFfi, MobileLinuxProcessHandleFfi, MobileLinuxPtyOpenRequestFfi,
    MobileLinuxPtySessionHandleFfi, MobileLinuxPtySizeFfi, MobileLinuxStatusFfi,
    MobileLinuxTaskSnapshotFfi, MobileLinuxTaskStateFfi, RawStdioHandle, RawStdioOutput,
    RuntimeConfig, RuntimePlatform,
};

const MAX_MOBILE_LINUX_EVENT_BATCH: usize = 512;

static MOBILE_LINUX_FFI_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(uniffi::Object)]
pub struct RuntimeHandle {
    runtime: Arc<dyn platform_api::MobileLinuxRuntime>,
}

#[uniffi::export(async_runtime = "tokio")]
impl RuntimeHandle {
    pub async fn capability(&self) -> MobileLinuxCapabilityFfi {
        capability_to_ffi(self.runtime.probe_capability().await)
    }

    pub async fn status(&self) -> Result<MobileLinuxStatusFfi, MobileLinuxApiErrorFfi> {
        self.runtime
            .rootfs_status()
            .await
            .map(status_to_ffi)
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn boot(&self) -> Result<MobileLinuxStatusFfi, MobileLinuxApiErrorFfi> {
        self.runtime
            .boot()
            .await
            .map(status_to_ffi)
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn shutdown(&self) -> Result<(), MobileLinuxApiErrorFfi> {
        self.runtime
            .shutdown()
            .await
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn verify_rootfs(&self) -> Result<MobileLinuxStatusFfi, MobileLinuxApiErrorFfi> {
        self.runtime
            .verify_rootfs()
            .await
            .map(status_to_ffi)
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn repair_rootfs(&self) -> Result<MobileLinuxStatusFfi, MobileLinuxApiErrorFfi> {
        self.runtime
            .repair_rootfs()
            .await
            .map(status_to_ffi)
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn reset_rootfs(&self) -> Result<MobileLinuxStatusFfi, MobileLinuxApiErrorFfi> {
        self.runtime
            .reset_rootfs()
            .await
            .map(status_to_ffi)
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn run_command(
        &self,
        request: MobileLinuxCommandRequestFfi,
    ) -> Result<MobileLinuxCommandResultFfi, MobileLinuxApiErrorFfi> {
        self.runtime
            .run(command_request_to_traits(request)?)
            .await
            .map(command_result_to_ffi)
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn run_command_streaming(
        &self,
        request: MobileLinuxCommandRequestFfi,
        sink: Box<dyn RuntimeEventSink>,
    ) -> Result<MobileLinuxCommandResultFfi, MobileLinuxApiErrorFfi> {
        let task_id = format!(
            "run-{}",
            MOBILE_LINUX_FFI_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let sink = Arc::new(RuntimeEventSinkBridge {
            task_id,
            inner: sink,
        });
        let result = self
            .runtime
            .run_streaming(command_request_to_traits(request)?, sink.clone())
            .await
            .map_err(mobile_linux_error_to_ffi)?;
        sink.emit_event(MobileLinuxEventFfi {
            sequence: 0,
            task_id: Some(sink.task_id.clone()),
            kind: MobileLinuxEventKindFfi::TaskStatusChanged,
            data: None,
            text: None,
            session_id: None,
            status: Some(if result.cancelled {
                MobileLinuxTaskStateFfi::Cancelled
            } else if result.timed_out {
                MobileLinuxTaskStateFfi::TimedOut
            } else if result.exit_code == 0 {
                MobileLinuxTaskStateFfi::Completed
            } else {
                MobileLinuxTaskStateFfi::Failed
            }),
            exit_code: Some(result.exit_code),
            timed_out: Some(result.timed_out),
            cancelled: Some(result.cancelled),
            detail: None,
        })
        .await?;
        Ok(command_result_to_ffi(result))
    }

    pub async fn spawn_background(
        &self,
        request: MobileLinuxCommandRequestFfi,
    ) -> Result<MobileLinuxProcessHandleFfi, MobileLinuxApiErrorFfi> {
        self.runtime
            .spawn_background(command_request_to_traits(request)?)
            .await
            .map(process_handle_to_ffi)
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn kill_process(
        &self,
        handle: MobileLinuxProcessHandleFfi,
    ) -> Result<(), MobileLinuxApiErrorFfi> {
        self.runtime
            .kill(&process_handle_to_traits(handle)?)
            .await
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn open_pty(
        &self,
        request: MobileLinuxPtyOpenRequestFfi,
    ) -> Result<MobileLinuxPtySessionHandleFfi, MobileLinuxApiErrorFfi> {
        self.runtime
            .open_pty(pty_open_request_to_traits(request)?)
            .await
            .map(pty_handle_to_ffi)
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn write_pty(
        &self,
        handle: MobileLinuxPtySessionHandleFfi,
        input: Vec<u8>,
    ) -> Result<(), MobileLinuxApiErrorFfi> {
        self.runtime
            .write_pty(&pty_handle_to_traits(handle)?, input)
            .await
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn resize_pty(
        &self,
        handle: MobileLinuxPtySessionHandleFfi,
        size: MobileLinuxPtySizeFfi,
    ) -> Result<(), MobileLinuxApiErrorFfi> {
        self.runtime
            .resize_pty(
                &pty_handle_to_traits(handle)?,
                platform_api::PtySize {
                    cols: size.cols,
                    rows: size.rows,
                },
            )
            .await
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn close_pty(
        &self,
        handle: MobileLinuxPtySessionHandleFfi,
    ) -> Result<(), MobileLinuxApiErrorFfi> {
        self.runtime
            .close_pty(&pty_handle_to_traits(handle)?)
            .await
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn configure_mounts(
        &self,
        mounts: Vec<MobileLinuxMountSpecFfi>,
    ) -> Result<MobileLinuxStatusFfi, MobileLinuxApiErrorFfi> {
        let mounts = mounts
            .into_iter()
            .map(mount_spec_to_traits)
            .collect::<Result<Vec<_>, _>>()?;
        self.runtime
            .configure_mounts(mounts)
            .await
            .map_err(mobile_linux_error_to_ffi)?;
        self.status().await
    }

    pub async fn read_events(
        &self,
        after_sequence: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Vec<MobileLinuxEventFfi>, MobileLinuxApiErrorFfi> {
        let limit = limit
            .map(|value| value as usize)
            .unwrap_or(MAX_MOBILE_LINUX_EVENT_BATCH)
            .min(MAX_MOBILE_LINUX_EVENT_BATCH);
        self.runtime
            .read_events(after_sequence, limit)
            .await
            .map(|events| events.into_iter().map(event_to_ffi).collect())
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn list_tasks(
        &self,
    ) -> Result<Vec<MobileLinuxTaskSnapshotFfi>, MobileLinuxApiErrorFfi> {
        self.runtime
            .list_tasks()
            .await
            .map(|tasks| tasks.into_iter().map(task_snapshot_to_ffi).collect())
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn task_status(
        &self,
        task_id: String,
    ) -> Result<Option<MobileLinuxTaskSnapshotFfi>, MobileLinuxApiErrorFfi> {
        if task_id.trim().is_empty() {
            return Err(MobileLinuxApiErrorFfi::InvalidRequest {
                detail: "task_id must not be empty".to_string(),
            });
        }
        self.runtime
            .task_status(&task_id)
            .await
            .map(|task| task.map(task_snapshot_to_ffi))
            .map_err(mobile_linux_error_to_ffi)
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl RuntimeHandle {
    pub async fn open_raw_stdio(
        &self,
        request: MobileLinuxCommandRequestFfi,
    ) -> Result<RawStdioHandle, MobileLinuxApiErrorFfi> {
        if request.stdin.is_some() || request.timeout_ms.is_some() {
            return Err(MobileLinuxApiErrorFfi::InvalidRequest {
                detail: "Raw stdio writes and lifetime are controlled through the session handle"
                    .into(),
            });
        }
        let r = command_request_to_traits(request)?;
        let h = self
            .runtime
            .open_raw_stdio(platform_api::RawStdioOpenRequest {
                command: r.command,
                args: r.args,
                cwd: r.cwd,
                env: r.env,
                network: r.network,
                resource_limits: r.resource_limits,
                mounts: r.mounts,
            })
            .await
            .map_err(mobile_linux_error_to_ffi)?;
        Ok(RawStdioHandle {
            id: h.id,
            enforcement: h.enforcement.into(),
        })
    }
    pub async fn write_raw_stdio(
        &self,
        id: String,
        data: Vec<u8>,
    ) -> Result<(), MobileLinuxApiErrorFfi> {
        self.runtime
            .write_raw_stdio(&raw_handle(id)?, data)
            .await
            .map_err(mobile_linux_error_to_ffi)
    }
    pub async fn read_raw_stdio(
        &self,
        id: String,
        max_bytes: u32,
    ) -> Result<RawStdioOutput, MobileLinuxApiErrorFfi> {
        if max_bytes == 0 || max_bytes > 1048576 {
            return Err(MobileLinuxApiErrorFfi::InvalidRequest {
                detail: "max_bytes must be 1..1048576".into(),
            });
        }
        let v = self
            .runtime
            .read_raw_stdio(&raw_handle(id)?, max_bytes as usize)
            .await
            .map_err(mobile_linux_error_to_ffi)?;
        Ok(RawStdioOutput {
            stdout: v.stdout,
            stderr: v.stderr,
            closed: v.closed,
            exit_code: v.exit_code,
        })
    }
    pub async fn close_raw_stdio(&self, id: String) -> Result<(), MobileLinuxApiErrorFfi> {
        self.runtime
            .close_raw_stdio(&raw_handle(id)?)
            .await
            .map_err(mobile_linux_error_to_ffi)
    }
}

#[uniffi::export]
pub fn create_runtime(config: RuntimeConfig) -> Result<Arc<RuntimeHandle>, MobileLinuxApiErrorFfi> {
    for value in [&config.managed_root, &config.app_sandbox_root] {
        if !std::path::Path::new(value).is_absolute() {
            return Err(MobileLinuxApiErrorFfi::InvalidRequest {
                detail: "runtime roots must be absolute".into(),
            });
        }
    }
    if config.rootfs_version.trim().is_empty() || config.abi.trim().is_empty() {
        return Err(MobileLinuxApiErrorFfi::InvalidRequest {
            detail: "rootfs version and ABI are required".into(),
        });
    }
    #[cfg(target_os = "android")]
    if matches!(config.platform, RuntimePlatform::Android) {
        let runtime = mobile_linux_android::AndroidProotRuntime::new(
            mobile_linux_android::AndroidProotRuntimeConfig {
                managed_root: PathBuf::from(&config.managed_root),
                app_sandbox_root: PathBuf::from(&config.app_sandbox_root),
                abi: config.abi.clone(),
                rootfs_version: config.rootfs_version.clone(),
                archive_sha256: config.archive_sha256.clone(),
                native_library_dir: config.native_library_dir.clone().map(PathBuf::from),
            },
        );
        return Ok(Arc::new(RuntimeHandle {
            runtime: Arc::new(runtime),
        }));
    }
    create_ios_or_unavailable(config)
}

pub(crate) fn create_ios_or_unavailable(
    config: RuntimeConfig,
) -> Result<Arc<RuntimeHandle>, MobileLinuxApiErrorFfi> {
    #[cfg(target_os = "ios")]
    if matches!(config.platform, RuntimePlatform::Ios) {
        let workspace = config
            .workspace_host_path
            .as_ref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| MobileLinuxApiErrorFfi::InvalidRequest {
                detail: "iOS requires an explicit workspace_host_path".into(),
            })?;
        let stable_id = config
            .stable_workspace_id
            .as_ref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| MobileLinuxApiErrorFfi::InvalidRequest {
                detail: "iOS requires an explicit stable_workspace_id".into(),
            })?;
        let runtime = mobile_linux_ios::linked_runtime(mobile_linux_ios::IosIshRuntimeConfig {
            managed_root: PathBuf::from(&config.managed_root),
            app_sandbox_root: PathBuf::from(&config.app_sandbox_root),
            workspace_host_path: PathBuf::from(workspace),
            stable_workspace_id: stable_id.clone(),
            abi: config.abi,
            rootfs_version: config.rootfs_version,
            archive_sha256: config.archive_sha256,
            authorization_file: config.authorization_file,
            rootfs_archive_path: config.rootfs_archive_path.map(PathBuf::from),
            rootfs_patch_path: config.rootfs_patch_path.map(PathBuf::from),
            default_mount_path: config.default_mount_path.map(PathBuf::from),
            protected_host_roots: config
                .protected_host_roots
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            allowed_mount_roots: config
                .allowed_mount_roots
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            allowed_guest_roots: config.allowed_guest_roots,
        })
        .map_err(mobile_linux_error_to_ffi)?;
        return Ok(Arc::new(RuntimeHandle { runtime }));
    }
    let (backend, name) = match config.platform {
        RuntimePlatform::Android => (platform_api::SandboxBackend::AndroidProot, "android"),
        RuntimePlatform::Ios => (platform_api::SandboxBackend::IosIsh, "ios"),
    };
    Ok(Arc::new(RuntimeHandle {
        runtime: Arc::new(platform_api::UnavailableMobileLinuxRuntime::unavailable(
            backend,
            platform_api::MobileLinuxRuntimeMode::MobileLinux,
            name,
            config.abi,
            "runtime execution requires a supported physical device",
        )),
    }))
}
