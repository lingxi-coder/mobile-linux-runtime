//! Consumer checks for public Windows handle operations.

#![cfg(windows)]

use platform_pty::{
    delete_windows_path_by_handle, enumerate_directory_by_handle, open_directory_entry_by_handle,
    open_windows_reparse_guarded, windows_file_identity, WindowsDirectoryEntry,
};

#[test]
fn directory_file_ids_reopen_and_delete_the_enumerated_file() -> std::io::Result<()> {
    let temp = tempfile::tempdir()?;
    let child_path = temp.path().join("child.txt");
    std::fs::write(&child_path, b"anchored child")?;
    let directory =
        open_windows_reparse_guarded(temp.path(), false).expect("open the parent directory");
    let entries: Vec<WindowsDirectoryEntry> =
        enumerate_directory_by_handle(&directory).expect("enumerate file identities");
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].is_directory);
    let child = open_directory_entry_by_handle(&directory, &entries[0], true)
        .expect("reopen the enumerated identity");
    let by_path =
        open_windows_reparse_guarded(&child_path, false).expect("open the same file by path");
    assert_eq!(
        windows_file_identity(&child)?,
        windows_file_identity(&by_path)?
    );
    drop(by_path);
    delete_windows_path_by_handle(&child).expect("mark the opened child for deletion");
    drop(child);
    assert!(!child_path.exists());
    Ok(())
}

#[test]
fn empty_directory_is_deleted_after_handle_close() -> std::io::Result<()> {
    let temp = tempfile::tempdir()?;
    let child_path = temp.path().join("child-directory");
    std::fs::create_dir(&child_path)?;
    let child = open_windows_reparse_guarded(&child_path, true)?;
    delete_windows_path_by_handle(&child).expect("mark the empty directory for deletion");
    drop(child);
    assert!(!child_path.exists());
    Ok(())
}

#[test]
fn deleting_without_delete_access_fails_and_preserves_the_file() -> std::io::Result<()> {
    let temp = tempfile::tempdir()?;
    let child_path = temp.path().join("read-only-handle.txt");
    std::fs::write(&child_path, b"preserved")?;
    let child = open_windows_reparse_guarded(&child_path, false)?;
    assert_eq!(
        delete_windows_path_by_handle(&child)
            .expect_err("DELETE access is required")
            .kind(),
        std::io::ErrorKind::PermissionDenied,
    );
    drop(child);
    assert_eq!(std::fs::read(child_path)?, b"preserved");
    Ok(())
}

#[test]
fn replacing_an_enumerated_name_is_rejected() -> std::io::Result<()> {
    let temp = tempfile::tempdir()?;
    let child_path = temp.path().join("child.txt");
    std::fs::write(&child_path, b"original")?;
    let directory = open_windows_reparse_guarded(temp.path(), false)?;
    let entry = enumerate_directory_by_handle(&directory)?.remove(0);
    std::fs::rename(&child_path, temp.path().join("retained.txt"))?;
    std::fs::write(&child_path, b"replacement")?;
    assert!(open_directory_entry_by_handle(&directory, &entry, true).is_err());
    assert_eq!(std::fs::read(&child_path)?, b"replacement");
    assert_eq!(
        std::fs::read(temp.path().join("retained.txt"))?,
        b"original"
    );
    Ok(())
}

#[test]
fn renamed_parent_remains_anchored_and_external_hardlink_is_preserved() -> std::io::Result<()> {
    let temp = tempfile::tempdir()?;
    let parent_path = temp.path().join("parent");
    std::fs::create_dir(&parent_path)?;
    std::fs::write(parent_path.join("child.txt"), b"original")?;
    std::fs::hard_link(
        parent_path.join("child.txt"),
        temp.path().join("outside.txt"),
    )?;
    let directory = open_windows_reparse_guarded(&parent_path, false)?;
    let entry = enumerate_directory_by_handle(&directory)?.remove(0);
    let moved = temp.path().join("moved");
    std::fs::rename(&parent_path, &moved)?;
    std::fs::create_dir(&parent_path)?;
    std::fs::write(parent_path.join("child.txt"), b"replacement")?;
    let child = open_directory_entry_by_handle(&directory, &entry, true)?;
    delete_windows_path_by_handle(&child)?;
    drop(child);
    assert!(!moved.join("child.txt").exists());
    assert_eq!(std::fs::read(temp.path().join("outside.txt"))?, b"original");
    assert_eq!(
        std::fs::read(parent_path.join("child.txt"))?,
        b"replacement"
    );
    Ok(())
}

#[test]
fn entry_names_cannot_escape_the_parent_or_open_streams() -> std::io::Result<()> {
    let temp = tempfile::tempdir()?;
    let directory = open_windows_reparse_guarded(temp.path(), false)?;
    for name in [
        "",
        ".",
        "..",
        "../outside",
        "..\\outside",
        "C:\\outside",
        "child:stream",
        "bad\0name",
    ] {
        let entry = WindowsDirectoryEntry {
            name: name.into(),
            file_id: 0,
            is_directory: false,
        };
        assert_eq!(
            open_directory_entry_by_handle(&directory, &entry, true)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
    Ok(())
}
