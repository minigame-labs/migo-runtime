//! Descriptor-anchored regular-file opens for Unix VFS mounts.
//!
//! All path traversal below is rooted in an already-open directory descriptor.
//! This is deliberately separate from the VFS's textual/canonical path checks:
//! those checks are useful policy validation, while this module closes the
//! time-of-check/time-of-use window during the final read open.

use std::ffi::{CString, OsString};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use crate::vfs::VfsOpenError;

const ROOT_OPEN_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
const DIRECTORY_OPEN_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
const FILE_OPEN_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;

// openat2 resolves the whole path beneath the root in one call, on desktop Linux only. Android's and OpenHarmony's
// app sandboxes filter system calls with seccomp, and a call outside the filter's list is not refused with ENOSYS --
// the process is killed (SIGSYS): Android 14 kills an app that tries openat2. So nothing there may probe for it, and
// those platforms take the descriptor walk below, which is as tight -- every component opened beneath the last with
// O_NOFOLLOW -- at a few more calls.
//
// Linux UAPI `openat2.h` values.
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
const RESOLVE_BENEATH: u64 = 0x08;

/// Open a regular file below `base` without ever re-resolving an already
/// checked parent pathname.
pub(super) fn open_regular_for_read(root: &File, relative: &Path) -> Result<File, VfsOpenError> {
    let components = relative_components(relative)?;

    // Desktop Linux only; see RESOLVE_BENEATH.
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        match openat2_regular(root.as_raw_fd(), relative) {
            Ok(file) => return ensure_regular(file),
            // A kernel older than 5.6, or a container whose seccomp profile
            // answers ENOSYS, uses the descriptor walk below. Other errors
            // came from a fully constrained resolution and must be returned
            // as VFS errors.
            Err(Openat2Error::Unavailable) => {}
            Err(Openat2Error::Open(error)) => return Err(map_open_error(error)),
        }
    }

    let root = root.try_clone().map_err(map_open_error)?;
    open_by_descriptor_walk(root, &components, || {})
}

/// Test-only counterpart which runs `before_final` after every parent has
/// been opened, but immediately before the final descriptor-relative open.
///
/// It intentionally bypasses openat2, so tests can prove that replacing a
/// pathname after parent traversal cannot redirect the final open.
#[cfg(test)]
pub(super) fn open_regular_for_read_with_hook<F: FnOnce()>(
    root: &File,
    relative: &Path,
    before_final: F,
) -> Result<File, VfsOpenError> {
    let components = relative_components(relative)?;
    let root = root.try_clone().map_err(map_open_error)?;
    open_by_descriptor_walk(root, &components, before_final)
}

fn relative_components(relative: &Path) -> Result<Vec<OsString>, VfsOpenError> {
    let mut components = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(component) => components.push(component.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(VfsOpenError::UnsafePath);
            }
        }
    }

    if components.is_empty() {
        return Err(VfsOpenError::UnsafePath);
    }
    Ok(components)
}

pub(super) fn pin_root(base: &Path) -> Result<File, VfsOpenError> {
    let base = path_cstring(base)?;
    // SAFETY: `base` is NUL-terminated and remains alive for the call.
    let fd = unsafe { libc::open(base.as_ptr(), ROOT_OPEN_FLAGS) };
    fd_to_file(fd)
}

pub(super) fn capture_write_directory(
    root: &File,
    relative: &Path,
    create: bool,
) -> std::io::Result<File> {
    let mut parent = root.try_clone()?;
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unsafe directory component",
            ));
        };
        let name = CString::new(component.as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid directory name")
        })?;
        // SAFETY: the live directory and NUL-terminated component are borrowed
        // for this call; O_NOFOLLOW/O_DIRECTORY reject replaced symlinks/FIFOs.
        let mut fd =
            unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), DIRECTORY_OPEN_FLAGS) };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if !create || error.kind() != std::io::ErrorKind::NotFound {
                return Err(error);
            }
            // SAFETY: same pinned parent/name. A competing mkdir is harmless;
            // reopen below validates the actual object without following links.
            if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o777) } < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
            // SAFETY: identical directory-relative guarded open after mkdir.
            fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), DIRECTORY_OPEN_FLAGS) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        // SAFETY: openat just returned this owned descriptor exactly once.
        parent = unsafe { File::from_raw_fd(fd) };
    }
    Ok(parent)
}

pub(super) fn create_new_child(parent: &File, name: &str) -> std::io::Result<File> {
    let name = CString::new(name)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid child name"))?;
    let flags = libc::O_WRONLY
        | libc::O_CREAT
        | libc::O_EXCL
        | libc::O_NOFOLLOW
        | libc::O_CLOEXEC
        | libc::O_NONBLOCK;
    // SAFETY: the parent is retained and name is one validated component.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags, 0o666) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: ownership of the successful openat result is transferred once.
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(super) fn remove_child(parent: &File, name: &str) -> std::io::Result<()> {
    let name = CString::new(name)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid child name"))?;
    // SAFETY: parent/name are live; flags=0 unlinks the leaf itself and refuses
    // directories, without following a symlink to another object's contents.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn open_by_descriptor_walk<F: FnOnce()>(
    mut parent: File,
    components: &[OsString],
    before_final: F,
) -> Result<File, VfsOpenError> {
    let (final_component, parents) = components
        .split_last()
        .expect("relative_components rejects an empty path");

    for component in parents {
        parent = openat(parent.as_raw_fd(), component, DIRECTORY_OPEN_FLAGS)?;
    }

    before_final();
    let file = openat(parent.as_raw_fd(), final_component, FILE_OPEN_FLAGS)?;
    ensure_regular(file)
}

fn openat(
    parent_fd: libc::c_int,
    component: &std::ffi::OsStr,
    flags: libc::c_int,
) -> Result<File, VfsOpenError> {
    let component = os_str_cstring(component)?;
    // SAFETY: `parent_fd` is owned by `parent`, and `component` is a valid
    // NUL-terminated single path component for this call.
    let fd = unsafe { libc::openat(parent_fd, component.as_ptr(), flags) };
    fd_to_file(fd)
}

fn fd_to_file(fd: libc::c_int) -> Result<File, VfsOpenError> {
    if fd < 0 {
        return Err(map_open_error(std::io::Error::last_os_error()));
    }
    // SAFETY: a non-negative descriptor was just returned by open/openat and
    // ownership is transferred to the resulting File exactly once.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn ensure_regular(file: File) -> Result<File, VfsOpenError> {
    match file.metadata() {
        Ok(metadata) if metadata.file_type().is_file() => Ok(file),
        Ok(_) => Err(VfsOpenError::NotRegularFile),
        Err(error) => Err(map_open_error(error)),
    }
}

fn path_cstring(path: &Path) -> Result<CString, VfsOpenError> {
    os_str_cstring(path.as_os_str())
}

fn os_str_cstring(value: &std::ffi::OsStr) -> Result<CString, VfsOpenError> {
    CString::new(value.as_bytes()).map_err(|_| VfsOpenError::UnsafePath)
}

fn map_open_error(error: std::io::Error) -> VfsOpenError {
    match error.raw_os_error() {
        Some(libc::ENOENT) => VfsOpenError::NotFound,
        Some(libc::EACCES | libc::EPERM) => VfsOpenError::AccessDenied,
        // O_NOFOLLOW and RESOLVE_NO_SYMLINKS both report symlinks as ELOOP.
        Some(libc::ELOOP) => VfsOpenError::UnsafePath,
        _ => VfsOpenError::OpenFailed,
    }
}

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
enum Openat2Error {
    Unavailable,
    Open(std::io::Error),
}

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
fn openat2_regular(parent_fd: libc::c_int, relative: &Path) -> Result<File, Openat2Error> {
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }

    let relative = path_cstring(relative)
        .map_err(|_| Openat2Error::Open(std::io::Error::from_raw_os_error(libc::EINVAL)))?;
    let how = OpenHow {
        flags: FILE_OPEN_FLAGS as u64,
        mode: 0,
        resolve: RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS,
    };
    // SAFETY: `relative` and `how` stay alive throughout the system call, and
    // the structure matches Linux's `struct open_how` layout.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            parent_fd,
            relative.as_ptr(),
            &how as *const OpenHow,
            std::mem::size_of::<OpenHow>(),
        )
    } as libc::c_int;
    if fd >= 0 {
        // SAFETY: a successful syscall produced a fresh owned descriptor.
        return Ok(unsafe { File::from_raw_fd(fd) });
    }

    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENOSYS | libc::EINVAL) => Err(Openat2Error::Unavailable),
        _ => Err(Openat2Error::Open(error)),
    }
}
