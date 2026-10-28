//! A directory capability for reads from an immutable code generation.
//! Reuses VFS traversal; never treats a resolved pathname as open authority.
use super::{VfsOpenError, is_unsafe_windows_component, normalize_path};
use std::{
    fs::File,
    path::{Component, Path},
};

#[derive(Debug)]
pub struct ReadRoot {
    file: File,
}
impl ReadRoot {
    /// Full native identity; this capability keeps the directory object alive.
    pub fn identity(&self) -> std::io::Result<super::file_io::FileIdentity> {
        super::file_io::FileIdentity::of(&self.file)
    }
    /// Capture before publishing a directory mount. Missing/unsafe roots fail;
    /// a later pathname replacement cannot silently acquire this capability.
    pub fn open(path: &Path) -> Result<Self, VfsOpenError> {
        let path = normalize_path(path);
        #[cfg(unix)]
        let file = super::unix::pin_root(&path)?;
        #[cfg(windows)]
        let file = super::windows::pin_movable_root(&path)?;
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            return Err(VfsOpenError::OpenFailed);
        }
        #[cfg(any(unix, windows))]
        Ok(Self { file })
    }
    pub fn open_regular(&self, relative: &Path) -> Result<File, VfsOpenError> {
        validate_relative(relative)?;
        #[cfg(unix)]
        {
            super::unix::open_regular_for_read(&self.file, relative)
        }
        #[cfg(windows)]
        {
            super::windows::open_regular_from_movable_root(&self.file, relative)
        }
        #[cfg(not(any(unix, windows)))]
        Err(VfsOpenError::OpenFailed)
    }
    #[cfg(all(test, unix))]
    pub(super) fn open_with_hook(
        &self,
        relative: &Path,
        hook: impl FnOnce(),
    ) -> Result<File, VfsOpenError> {
        validate_relative(relative)?;
        super::unix::open_regular_for_read_with_hook(&self.file, relative, hook)
    }
    #[cfg(all(test, windows))]
    pub(super) fn open_with_guard_hook(
        &self,
        relative: &Path,
        hook: impl FnOnce(&Path),
    ) -> Result<File, VfsOpenError> {
        validate_relative(relative)?;
        super::windows::open_regular_from_movable_root_with_hook(&self.file, relative, hook)
    }
}
pub(super) fn validate_relative(relative: &Path) -> Result<(), VfsOpenError> {
    if relative.as_os_str().is_empty() {
        return Err(VfsOpenError::NotRegularFile);
    }
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(VfsOpenError::UnsafePath);
        };
        let name = name.to_str().ok_or(VfsOpenError::UnsafePath)?;
        if name
            .chars()
            .any(|c| c.is_control() || c == '\\' || c == ':')
            || is_unsafe_windows_component(name)
        {
            return Err(VfsOpenError::UnsafePath);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
