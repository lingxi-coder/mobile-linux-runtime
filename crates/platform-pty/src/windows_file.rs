use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;
use std::slice;

use winapi::shared::minwindef::{BOOL, DWORD};
use winapi::shared::ntdef::{LARGE_INTEGER, NTSTATUS, OBJECT_ATTRIBUTES, UNICODE_STRING};
use winapi::shared::winerror::ERROR_NO_MORE_FILES;
use winapi::um::fileapi::{
    GetFileInformationByHandle, SetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    FILE_ID_BOTH_DIR_INFO,
};
use winapi::um::minwinbase::{
    FileDispositionInfoEx, FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo,
};
use winapi::um::winbase::{
    GetFileInformationByHandleEx, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
};
use winapi::um::winnt::{
    DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_LIST_DIRECTORY,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GENERIC_READ, HANDLE, SYNCHRONIZE,
};

/// Stable volume and file identity for an opened Windows object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowsFileIdentity {
    /// Volume containing the file.
    pub volume_serial_number: u32,
    /// File identifier within the volume.
    pub file_index: u64,
}

/// File identity returned by handle-based directory enumeration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsDirectoryEntry {
    /// Identifier used to reopen this object on the parent volume.
    pub file_id: i64,
    /// Exact single-component name returned by directory enumeration.
    pub name: OsString,
    /// Whether the directory entry has the directory attribute.
    pub is_directory: bool,
}

/// Open a path without following reparse points and verify its identity.
pub fn open_reparse_guarded(path: &Path, delete_access: bool) -> io::Result<File> {
    let mut access = GENERIC_READ | FILE_LIST_DIRECTORY;
    if delete_access {
        access |= DELETE;
    }
    let file = std::fs::OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let identity = file_identity(&file)?;
    let _ = identity;
    Ok(file)
}

/// Enumerate object identities from an already opened directory handle.
pub fn enumerate_directory_by_handle(dir: &File) -> io::Result<Vec<WindowsDirectoryEntry>> {
    let mut entries = Vec::new();
    let mut restart = true;
    loop {
        // Windows requires directory records to be aligned to eight bytes.
        let mut buf = vec![0u64; 64 * 1024 / size_of::<u64>()];
        let class = if restart {
            FileIdBothDirectoryRestartInfo
        } else {
            FileIdBothDirectoryInfo
        };
        let ok = unsafe {
            GetFileInformationByHandleEx(
                dir.as_raw_handle() as HANDLE,
                class,
                buf.as_mut_ptr().cast(),
                DWORD::try_from(buf.len() * size_of::<u64>()).unwrap_or(u32::MAX),
            )
        };
        if ok == 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                break;
            }
            return Err(err);
        }
        restart = false;
        let mut offset = 0usize;
        loop {
            let entry = unsafe {
                &*(buf
                    .as_ptr()
                    .cast::<u8>()
                    .add(offset)
                    .cast::<FILE_ID_BOTH_DIR_INFO>())
            };
            let name_len = usize::try_from(entry.FileNameLength / 2).unwrap_or(0);
            let name_ptr = entry.FileName.as_ptr();
            let name = unsafe { slice::from_raw_parts(name_ptr, name_len) };
            if name != ['.' as u16] && name != ['.' as u16, '.' as u16] {
                entries.push(WindowsDirectoryEntry {
                    file_id: unsafe { *entry.FileId.QuadPart() },
                    name: OsString::from_wide(name),
                    is_directory: entry.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0,
                });
            }
            if entry.NextEntryOffset == 0 {
                break;
            }
            offset += usize::try_from(entry.NextEntryOffset).unwrap_or(0);
        }
    }
    Ok(entries)
}

// User-mode NT declarations keep the parent handle in the open operation;
// CreateFileW has no directory-relative variant. Layout follows winternl.h.
#[repr(C)]
struct IoStatusBlock {
    status_or_pointer: usize,
    information: usize,
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtCreateFile(
        handle: *mut HANDLE,
        access: DWORD,
        attributes: *mut OBJECT_ATTRIBUTES,
        status: *mut IoStatusBlock,
        allocation_size: *mut LARGE_INTEGER,
        file_attributes: DWORD,
        share: DWORD,
        disposition: DWORD,
        options: DWORD,
        ea_buffer: *mut std::ffi::c_void,
        ea_length: DWORD,
    ) -> NTSTATUS;
    fn RtlNtStatusToDosError(status: NTSTATUS) -> DWORD;
}

/// Open an enumerated child relative to its retained parent and verify its ID.
///
/// Windows cannot unlink an OpenFileById handle. A name relative to the parent
/// selects the directory link to delete; checking its ID after opening rejects
/// replacement between enumeration and opening. No reparse point is followed.
pub fn open_directory_entry_by_handle(
    parent: &File,
    entry: &WindowsDirectoryEntry,
    delete_access: bool,
) -> io::Result<File> {
    let mut name: Vec<u16> = entry.name.encode_wide().collect();
    if name.is_empty()
        || name == ['.' as u16]
        || name == ['.' as u16, '.' as u16]
        || name
            .iter()
            .any(|c| [0, '/' as u16, '\\' as u16, ':' as u16].contains(c))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected one directory-entry name",
        ));
    }
    let length = u16::try_from(name.len() * size_of::<u16>()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory-entry name is too long",
        )
    })?;
    let mut object_name = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: name.as_mut_ptr(),
    };
    let mut attributes: OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
    attributes.Length = DWORD::try_from(size_of::<OBJECT_ATTRIBUTES>()).unwrap_or(u32::MAX);
    attributes.RootDirectory = parent.as_raw_handle() as HANDLE;
    attributes.ObjectName = &mut object_name;
    let mut status_block = IoStatusBlock {
        status_or_pointer: 0,
        information: 0,
    };
    let mut handle: HANDLE = std::ptr::null_mut();
    let access = GENERIC_READ | SYNCHRONIZE | if delete_access { DELETE } else { 0 };
    const FILE_OPEN: DWORD = 1;
    const FILE_SYNCHRONOUS_IO_NONALERT: DWORD = 0x20;
    const FILE_OPEN_FOR_BACKUP_INTENT: DWORD = 0x4000;
    const FILE_OPEN_REPARSE_POINT: DWORD = 0x0020_0000;
    // All buffers live through the synchronous call. Only one validated name
    // component is resolved under RootDirectory, and the leaf is not followed.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access,
            &mut attributes,
            &mut status_block,
            std::ptr::null_mut(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_FOR_BACKUP_INTENT | FILE_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
            0,
        )
    };
    if status < 0 {
        return Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ));
    }
    if handle.is_null() || handle == (-1isize) as HANDLE {
        return Err(io::Error::other("NtCreateFile returned no child handle"));
    }
    let file = unsafe { File::from_raw_handle(handle.cast()) };
    let actual = file_identity(&file)?;
    if actual.volume_serial_number != file_identity(parent)?.volume_serial_number
        || actual.file_index != entry.file_id as u64
    {
        return Err(io::Error::other(
            "directory entry identity changed before opening",
        ));
    }
    Ok(file)
}

/// Read the stable identity of a handle, refusing reparse points.
pub fn file_identity(file: &File) -> io::Result<WindowsFileIdentity> {
    let mut info = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    let ok: BOOL = unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::other("managed worktree path is a reparse point"));
    }
    Ok(WindowsFileIdentity {
        volume_serial_number: info.dwVolumeSerialNumber,
        file_index: ((u64::from(info.nFileIndexHigh)) << 32) | u64::from(info.nFileIndexLow),
    })
}

/// Mark an opened object for deletion when its final handle closes.
pub fn delete_by_handle(file: &File) -> io::Result<()> {
    // ConPTY already requires Windows 10 1809. Use the DWORD-based extended
    // disposition class supported there with its exact DWORD buffer layout.
    // DELETE alone preserves deletion on final close, without POSIX semantics
    // or overriding read-only attributes.
    #[repr(C)]
    struct FileDispositionInfoExBuffer {
        flags: DWORD,
    }
    let mut disposition = FileDispositionInfoExBuffer { flags: 1 };
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as HANDLE,
            FileDispositionInfoEx,
            (&mut disposition as *mut FileDispositionInfoExBuffer).cast(),
            DWORD::try_from(size_of::<FileDispositionInfoExBuffer>()).unwrap_or(u32::MAX),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
