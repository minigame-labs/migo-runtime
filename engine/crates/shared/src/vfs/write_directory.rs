//! Single-child mutations beneath a captured writable VFS directory.
//! This holds namespace authority, not an immutable-content promise. Callers
//! still enroll a created File in FileMutations before writing its payload.
use std::{fs::File, io, path::Path};

#[derive(Debug)]
pub struct WriteDirectory {
    #[cfg(unix)]
    file: File,
    #[cfg(windows)]
    namespace: super::windows::WriteNamespace,
}

impl WriteDirectory {
    pub(super) fn capture(root: &File, relative: &Path, create: bool) -> io::Result<Self> {
        if !relative.as_os_str().is_empty() {
            super::read_root::validate_relative(relative).map_err(native_error)?;
        }
        #[cfg(unix)]
        return Ok(Self {
            file: super::unix::capture_write_directory(root, relative, create)?,
        });
        #[cfg(windows)]
        return Ok(Self {
            namespace: super::windows::capture_write_directory(root, relative, create)
                .map_err(native_error)?,
        });
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (root, relative, create);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "directory mutation unavailable",
            ))
        }
    }

    pub fn create_new(&self, name: &str) -> io::Result<File> {
        validate_name(name)?;
        #[cfg(unix)]
        return super::unix::create_new_child(&self.file, name);
        #[cfg(windows)]
        return self.namespace.create_new(name);
        #[cfg(not(any(unix, windows)))]
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory mutation unavailable",
        ))
    }

    /// Unlink this directory's child, including a leaf symlink itself. Never
    /// follow the child, traverse a path, or recursively remove a directory.
    pub fn remove_file(&self, name: &str) -> io::Result<()> {
        validate_name(name)?;
        #[cfg(unix)]
        return super::unix::remove_child(&self.file, name);
        #[cfg(windows)]
        return self.namespace.remove_file(name);
        #[cfg(not(any(unix, windows)))]
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory mutation unavailable",
        ))
    }
}

fn validate_name(name: &str) -> io::Result<()> {
    let path = Path::new(name);
    let mut components = path.components();
    if !matches!(components.next(), Some(std::path::Component::Normal(part)) if part == path.as_os_str())
        || components.next().is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected one child name",
        ));
    }
    super::read_root::validate_relative(path).map_err(native_error)
}

fn native_error(error: super::VfsOpenError) -> io::Error {
    let kind = match error {
        super::VfsOpenError::NotFound => io::ErrorKind::NotFound,
        super::VfsOpenError::AccessDenied => io::ErrorKind::PermissionDenied,
        super::VfsOpenError::UnsafePath | super::VfsOpenError::Policy(_) => {
            io::ErrorKind::InvalidInput
        }
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, error)
}

#[cfg(test)]
mod tests;
