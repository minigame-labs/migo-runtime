//! Windows handle-anchored strict VFS reads.
//!
//! This deliberately validates the objects represented by already-open
//! handles.  In particular, it never uses metadata collected from a path as
//! authority for a later open of that path.

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, PathBuf};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_SEQUENTIAL_SCAN,
    FILE_NAME_NORMALIZED, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK,
    GetFileInformationByHandle, GetFileType, GetFinalPathNameByHandleW, VOLUME_NAME_NT,
};

use crate::vfs::VfsOpenError;

const CSTR_EQUAL: i32 = 2;

// `CompareStringOrdinal` is the Windows definition of an ordinal,
// case-insensitive UTF-16 comparison.  windows-sys exposes it behind the
// optional Win32_Globalization feature; declaring this one kernel32 import
// locally keeps this module's dependency limited to the FileSystem API set.
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CompareStringOrdinal(
        string1: *const u16,
        count1: i32,
        string2: *const u16,
        count2: i32,
        ignore_case: i32,
    ) -> i32;
}

/// Open a regular file below `base` without following reparse points.
///
/// Each ancestor is kept open without `FILE_SHARE_DELETE` while the next
/// component is opened.  This pins the namespace traversed by the path until
/// the final file handle has been verified and returned.
pub(super) fn open_regular_for_read(
    root: &File,
    base: &Path,
    relative: &Path,
) -> Result<File, VfsOpenError> {
    let components = relative_components(relative)?;

    let mut parent_handles = Vec::with_capacity(components.len());
    let mut current = base.to_path_buf();
    validate_directory(root)?;
    let mut expected = final_path(root)?;

    let (last, parents) = components
        .split_last()
        // `relative_components` rejects an empty path.  Keep the error static
        // if that invariant is ever changed.
        .ok_or(VfsOpenError::UnsafePath)?;

    for component in parents {
        current.push(component);
        let directory = open_directory(&current)?;
        validate_directory(&directory)?;

        expected = expected_child_path(&expected, component);
        if !path_equals_ordinal_ignore_case(&final_path(&directory)?, &expected) {
            return Err(VfsOpenError::UnsafePath);
        }

        parent_handles.push(directory);
    }

    current.push(last);
    let file = open_final_file(&current)?;
    validate_regular_file(&file)?;

    let expected_final = expected_child_path(&expected, last);
    if !path_equals_ordinal_ignore_case(&final_path(&file)?, &expected_final) {
        return Err(VfsOpenError::UnsafePath);
    }

    // `parent_handles` remains in scope (and therefore keeps every ancestor
    // open without delete sharing) through the final handle validation above.
    Ok(file)
}

fn relative_components(relative: &Path) -> Result<Vec<OsString>, VfsOpenError> {
    let mut components = Vec::new();

    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(VfsOpenError::UnsafePath);
        };

        // Colons select an alternate data stream (or other drive/device
        // syntax) on Windows.  Do not permit any such name below a VFS root.
        if name.encode_wide().any(|unit| unit == b':' as u16) {
            return Err(VfsOpenError::UnsafePath);
        }
        let name_utf8 = name.to_str().ok_or(VfsOpenError::UnsafePath)?;
        if super::is_unsafe_windows_component(name_utf8) {
            return Err(VfsOpenError::UnsafePath);
        }
        components.push(name.to_os_string());
    }

    if components.is_empty() {
        return Err(VfsOpenError::UnsafePath);
    }
    Ok(components)
}

fn open_directory(path: &Path) -> Result<File, VfsOpenError> {
    let path = verbatim_path(path)?;
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(map_open_error)
}

/// Open and validate a root directory once so [`VirtualFS`](super::VirtualFS)
/// can hold the handle without delete sharing for its entire lifetime.
pub(super) fn pin_root(path: &Path) -> Result<File, VfsOpenError> {
    let root = open_directory(path)?;
    validate_directory(&root)?;
    final_path(&root)?;
    Ok(root)
}

fn open_final_file(path: &Path) -> Result<File, VfsOpenError> {
    let path = verbatim_path(path)?;
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_SEQUENTIAL_SCAN)
        .open(path)
        .map_err(map_open_error)
}

/// Convert a trusted absolute root-derived path into Win32's verbatim
/// namespace before `CreateFileW` sees it. A path that is not already verbatim
/// is normalized exactly as Win32 would normalize it, then fixed in that
/// spelling; this also prevents DOS aliases such as `NUL` from being
/// interpreted as devices if a future caller bypasses the portable component
/// guard.
fn verbatim_path(path: &Path) -> Result<PathBuf, VfsOpenError> {
    const SEP: u16 = b'\\' as u16;
    const SLASH: u16 = b'/' as u16;
    const QUESTION: u16 = b'?' as u16;
    const COLON: u16 = b':' as u16;

    let path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .map(|unit| if unit == SLASH { SEP } else { unit })
        .collect();
    if path.iter().any(|unit| *unit == 0) {
        return Err(VfsOpenError::UnsafePath);
    }

    let verbatim_prefix = [SEP, SEP, QUESTION, SEP];
    if path.starts_with(&verbatim_prefix) {
        let tail = &path[verbatim_prefix.len()..];
        let drive_absolute = tail.len() >= 3
            && tail[0] <= u8::MAX as u16
            && (tail[0] as u8).is_ascii_alphabetic()
            && tail[1] == COLON
            && tail[2] == SEP;
        let unc_absolute = tail.len() >= 4
            && tail[0] <= u8::MAX as u16
            && tail[1] <= u8::MAX as u16
            && tail[2] <= u8::MAX as u16
            && (tail[0] as u8).eq_ignore_ascii_case(&b'U')
            && (tail[1] as u8).eq_ignore_ascii_case(&b'N')
            && (tail[2] as u8).eq_ignore_ascii_case(&b'C')
            && tail[3] == SEP;
        if !drive_absolute && !unc_absolute {
            return Err(VfsOpenError::UnsafePath);
        }
        return Ok(PathBuf::from(OsString::from_wide(&path)));
    }

    // Relative and drive-relative roots cannot define a stable production
    // sandbox, so they are refused here -- before the normalization below,
    // which would otherwise quietly resolve them against the process's
    // current directory.
    let drive_absolute = path.len() >= 3
        && path[0] <= u8::MAX as u16
        && (path[0] as u8).is_ascii_alphabetic()
        && path[1] == COLON
        && path[2] == SEP;
    if !drive_absolute && !path.starts_with(&[SEP, SEP]) {
        return Err(VfsOpenError::UnsafePath);
    }

    // The verbatim prefix turns Win32 path normalization OFF: under `\\?\`,
    // `..` and `.` are file names, and `CreateFileW` answers
    // ERROR_INVALID_NAME for them. A host root such as `C:\app\files\..\cache`
    // -- which every Win32 API reads as `C:\app\cache` -- therefore failed to
    // pin, and every session refused to start. So the path is first given the
    // meaning Win32 itself gives it (`GetFullPathNameW`, which is what
    // `std::path::absolute` calls on Windows), and only then made verbatim.
    // The result is classified again below: normalization can produce a
    // device path (`C:\x\NUL` -> `\\.\NUL` on older Windows), which stays
    // refused.
    let mut path: Vec<u16> = std::path::absolute(PathBuf::from(OsString::from_wide(&path)))
        .map_err(|_| VfsOpenError::UnsafePath)?
        .as_os_str()
        .encode_wide()
        .collect();

    let mut verbatim = verbatim_prefix.to_vec();
    if path.starts_with(&[SEP, SEP, b'.' as u16, SEP]) || path.starts_with(&verbatim_prefix) {
        return Err(VfsOpenError::UnsafePath);
    } else if path.starts_with(&[SEP, SEP]) {
        // `\\server\share` -> `\\?\UNC\server\share`.
        verbatim.extend("UNC\\".encode_utf16());
        verbatim.extend_from_slice(&path[2..]);
    } else if path.len() >= 3
        && path[0] <= u8::MAX as u16
        && (path[0] as u8).is_ascii_alphabetic()
        && path[1] == COLON
        && path[2] == SEP
    {
        verbatim.append(&mut path);
    } else {
        // Relative, drive-relative, and device-namespace roots cannot define a
        // stable production sandbox.
        return Err(VfsOpenError::UnsafePath);
    }

    Ok(PathBuf::from(OsString::from_wide(&verbatim)))
}

fn map_open_error(error: io::Error) -> VfsOpenError {
    match error.kind() {
        io::ErrorKind::NotFound => VfsOpenError::NotFound,
        io::ErrorKind::PermissionDenied => VfsOpenError::AccessDenied,
        _ => VfsOpenError::OpenFailed,
    }
}

fn validate_directory(file: &File) -> Result<(), VfsOpenError> {
    let information = file_information(file)?;
    if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(VfsOpenError::UnsafePath);
    }
    if unsafe { GetFileType(handle(file)) } != FILE_TYPE_DISK {
        return Err(VfsOpenError::NotRegularFile);
    }
    if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Err(VfsOpenError::NotRegularFile);
    }
    Ok(())
}

fn validate_regular_file(file: &File) -> Result<(), VfsOpenError> {
    let information = file_information(file)?;
    if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(VfsOpenError::UnsafePath);
    }
    if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        return Err(VfsOpenError::NotRegularFile);
    }
    if unsafe { GetFileType(handle(file)) } != FILE_TYPE_DISK {
        return Err(VfsOpenError::NotRegularFile);
    }
    Ok(())
}

fn file_information(file: &File) -> Result<BY_HANDLE_FILE_INFORMATION, VfsOpenError> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(handle(file), &mut information) } == 0 {
        return Err(VfsOpenError::OpenFailed);
    }
    Ok(information)
}

fn final_path(file: &File) -> Result<Vec<u16>, VfsOpenError> {
    let flags = FILE_NAME_NORMALIZED | VOLUME_NAME_NT;
    let mut buffer = vec![0_u16; 512];

    loop {
        let written = unsafe {
            GetFinalPathNameByHandleW(
                handle(file),
                buffer.as_mut_ptr(),
                buffer
                    .len()
                    .try_into()
                    .map_err(|_| VfsOpenError::OpenFailed)?,
                flags,
            )
        };
        if written == 0 {
            return Err(VfsOpenError::OpenFailed);
        }
        if (written as usize) < buffer.len() {
            buffer.truncate(written as usize);
            return Ok(buffer);
        }

        let next_len = (written as usize)
            .checked_add(1)
            .ok_or(VfsOpenError::OpenFailed)?;
        buffer.resize(next_len, 0);
    }
}

fn handle(file: &File) -> HANDLE {
    file.as_raw_handle() as HANDLE
}

fn expected_child_path(parent: &[u16], child: &OsStr) -> Vec<u16> {
    let mut expected = parent.to_vec();
    if expected.last().copied() != Some(b'\\' as u16) {
        expected.push(b'\\' as u16);
    }
    expected.extend(child.encode_wide());
    expected
}

fn path_equals_ordinal_ignore_case(actual: &[u16], expected: &[u16]) -> bool {
    let mut actual_components = windows_path_components(actual);
    let mut expected_components = windows_path_components(expected);

    loop {
        match (actual_components.next(), expected_components.next()) {
            (Some(actual), Some(expected)) if ordinal_equal_ignore_case(actual, expected) => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

fn windows_path_components(path: &[u16]) -> impl Iterator<Item = &[u16]> {
    path.split(|unit| *unit == b'\\' as u16 || *unit == b'/' as u16)
        .filter(|component| !component.is_empty())
}

fn ordinal_equal_ignore_case(left: &[u16], right: &[u16]) -> bool {
    let Ok(left_len) = i32::try_from(left.len()) else {
        return false;
    };
    let Ok(right_len) = i32::try_from(right.len()) else {
        return false;
    };
    unsafe {
        CompareStringOrdinal(
            left.as_ptr(),
            left_len,
            right.as_ptr(),
            right_len,
            1, // TRUE
        ) == CSTR_EQUAL
    }
}
