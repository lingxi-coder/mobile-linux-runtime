//! Symlink-aware operations on managed rootfs trees.
use crate::RootfsStoreError;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

pub(crate) fn ensure_real_directory(path: &Path) -> Result<(), RootfsStoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|err| RootfsStoreError::Io {
        path: path.to_path_buf(),
        message: err.to_string(),
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(RootfsStoreError::UnsafeManagedPath(path.to_path_buf()));
    }
    Ok(())
}

pub(crate) fn path_present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

pub(crate) fn sha256_bytes(value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value);
    format!("{:x}", hasher.finalize())
}

pub(crate) fn sha256_file(path: &Path) -> Result<String, RootfsStoreError> {
    let mut file = fs::File::open(path).map_err(|err| RootfsStoreError::Io {
        path: path.to_path_buf(),
        message: err.to_string(),
    })?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 8192];
    loop {
        let read = file.read(&mut buf).map_err(|err| RootfsStoreError::Io {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(crate) fn dir_size(path: PathBuf) -> Result<u64, RootfsStoreError> {
    if !path.exists() {
        return Ok(0);
    }
    let metadata = fs::symlink_metadata(&path).map_err(|err| RootfsStoreError::Io {
        path: path.clone(),
        message: err.to_string(),
    })?;
    if metadata.file_type().is_symlink() {
        return Ok(0);
    }
    if metadata.is_file() {
        return fs::metadata(&path)
            .map(|metadata| metadata.len())
            .map_err(|err| RootfsStoreError::Io {
                path,
                message: err.to_string(),
            });
    }
    let mut total = 0;
    for entry in fs::read_dir(&path).map_err(|err| RootfsStoreError::Io {
        path: path.clone(),
        message: err.to_string(),
    })? {
        let entry = entry.map_err(|err| RootfsStoreError::Io {
            path: path.clone(),
            message: err.to_string(),
        })?;
        total += dir_size(entry.path())?;
    }
    Ok(total)
}

pub(crate) fn remove_path(path: PathBuf) -> Result<(), RootfsStoreError> {
    let metadata = fs::symlink_metadata(&path).map_err(|err| RootfsStoreError::Io {
        path: path.clone(),
        message: err.to_string(),
    })?;
    if metadata.file_type().is_symlink() {
        fs::remove_file(&path).map_err(|err| RootfsStoreError::Io {
            path,
            message: err.to_string(),
        })
    } else if metadata.is_dir() {
        for entry in fs::read_dir(&path).map_err(|err| RootfsStoreError::Io {
            path: path.clone(),
            message: err.to_string(),
        })? {
            let entry = entry.map_err(|err| RootfsStoreError::Io {
                path: path.clone(),
                message: err.to_string(),
            })?;
            remove_path(entry.path())?;
        }
        fs::remove_dir(&path).map_err(|err| RootfsStoreError::Io {
            path,
            message: err.to_string(),
        })
    } else {
        fs::remove_file(&path).map_err(|err| RootfsStoreError::Io {
            path,
            message: err.to_string(),
        })
    }
}
