//! Prepare a pinned source before opening the destination. Mutation authority
//! belongs to the actual destination File, never to a pathname or backend hook.
use shared::vfs::{
    file_copy::copy_contents, file_io::FileIdentity, file_mutation::FileMutations,
    mount::CopySource,
};
use std::{
    fs::{File, OpenOptions, Permissions},
    io::{self, Seek},
    path::Path,
};

pub(crate) struct PreparedCopy {
    source: CopySource,
    identity: Option<FileIdentity>,
    permissions: Option<Permissions>,
}
impl PreparedCopy {
    pub(crate) fn new(source: CopySource) -> io::Result<Self> {
        let (identity, permissions) = match &source {
            CopySource::File(file) => {
                let metadata = file.metadata()?;
                if !metadata.is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "copy source is not a regular file",
                    ));
                }
                // Apple fcopyfile already applies COPYFILE_METADATA. Avoid a
                // second chmod after native metadata/flags have been copied.
                let permissions = (!cfg!(target_vendor = "apple")).then(|| metadata.permissions());
                (Some(FileIdentity::of(file)?), permissions)
            }
            CopySource::Package(reader) => (Some(reader.backing_identity()?), None),
            CopySource::Stream(_) => (None, None),
        };
        Ok(Self {
            source,
            identity,
            permissions,
        })
    }
    pub(crate) fn copy_to_file(mut self, destination: &mut File) -> io::Result<()> {
        let mutations = FileMutations::for_file(destination)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "copy destination is not a regular file",
            )
        })?;
        if self.identity == Some(mutations.identity()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "copy source and destination are the same file",
            ));
        }
        let _write = mutations.into_write()?;
        destination.rewind()?;
        destination.set_len(0)?;
        match &mut self.source {
            CopySource::File(file) => copy_contents(file, destination)?,
            CopySource::Package(reader) => {
                reader.copy_to(destination).map_err(io::Error::other)?;
            }
            CopySource::Stream(reader) => {
                io::copy(reader, destination)?;
            }
        }
        if let Some(permissions) = self.permissions {
            destination.set_permissions(permissions)?;
        }
        Ok(())
    }
    pub(crate) fn copy_to_path(self, destination: &Path) -> io::Result<()> {
        self.copy_to_path_with(destination, |_| {})
    }
    fn copy_to_path_with(
        self,
        destination: &Path,
        after_open: impl FnOnce(&File),
    ) -> io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(false);
        nonblocking(&mut options);
        let mut destination = options.open(destination)?;
        after_open(&destination);
        self.copy_to_file(&mut destination)
    }
}
fn nonblocking(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    let _ = options;
}
pub(crate) fn open_source(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    nonblocking(&mut options);
    options.open(path)
}

#[cfg(test)]
mod tests;
