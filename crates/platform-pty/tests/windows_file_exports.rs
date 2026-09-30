//! Consumer checks for public Windows handle operations.

#![cfg(windows)]

use platform_pty::{
    delete_windows_path_by_handle, enumerate_directory_by_handle, open_child_by_id,
    open_windows_reparse_guarded, windows_file_identity, WindowsDirectoryEntry,
};

#[test]
fn directory_file_ids_reopen_and_delete_the_enumerated_file() -> std::io::Result<()> {
    let temp = tempfile::tempdir()?;
    let child_path = temp.path().join("child.txt");
    std::fs::write(&child_path, b"anchored child")?;
    let directory = open_windows_reparse_guarded(temp.path(), false).expect("open the parent directory");
    let entries: Vec<WindowsDirectoryEntry> = enumerate_directory_by_handle(&directory).expect("enumerate file identities");
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].is_directory);
    let child = open_child_by_id(&directory, entries[0].file_id, true).expect("reopen the enumerated identity");
    let by_path = open_windows_reparse_guarded(&child_path, false).expect("open the same file by path");
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
