use mobile_linux_api::{MobileLinuxError, MountSpec};
use std::collections::{BTreeMap, HashMap};

use super::{executable_regular_file, AndroidProotRuntime};

impl AndroidProotRuntime {
    pub(super) fn build_pty_invocation(
        &self,
        executable: &str,
        executable_args: &[String],
        cwd: Option<&str>,
        env: &BTreeMap<String, String>,
        mounts: &[MountSpec],
    ) -> Result<(String, Vec<String>, HashMap<String, String>), MobileLinuxError> {
        let (proot, rootfs) = self.readiness()?;
        let mut args = vec![
            "-0".to_string(),
            "--link2symlink".to_string(),
            "-r".to_string(),
            rootfs.display().to_string(),
            "-b".to_string(),
            "/dev".to_string(),
            "-b".to_string(),
            "/proc".to_string(),
            "-b".to_string(),
            "/sys".to_string(),
            "-w".to_string(),
            cwd.unwrap_or("/root").to_string(),
        ];
        for mount in mounts {
            args.push("-b".to_string());
            args.push(format!(
                "{}:{}",
                mount.host_path.display(),
                mount.guest_path
            ));
        }
        args.push(executable.to_string());
        args.extend(executable_args.iter().cloned());
        let mut child_env = HashMap::from([
            (
                "PROOT_TMP_DIR".to_string(),
                self.state
                    .config
                    .managed_root
                    .join("tmp")
                    .display()
                    .to_string(),
            ),
            (
                "PATH".to_string(),
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/opt/bin".to_string(),
            ),
            ("HOME".to_string(), "/root".to_string()),
        ]);
        child_env.extend(env.iter().map(|(key, value)| (key.clone(), value.clone())));
        if let Some(native_lib_dir) = proot.parent() {
            child_env.insert(
                "LD_LIBRARY_PATH".to_string(),
                native_lib_dir.display().to_string(),
            );
            let loader = native_lib_dir.join("libproot-loader.so");
            if executable_regular_file(&loader) {
                child_env.insert("PROOT_LOADER".to_string(), loader.display().to_string());
            }
            let loader32 = native_lib_dir.join("libproot-loader32.so");
            if executable_regular_file(&loader32) {
                child_env.insert(
                    "PROOT_LOADER_32".to_string(),
                    loader32.display().to_string(),
                );
            }
        }
        Ok((proot.display().to_string(), args, child_env))
    }
}
