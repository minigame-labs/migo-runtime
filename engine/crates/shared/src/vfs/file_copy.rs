//! Copy bytes between the actual opened regular file objects. Callers reject
//! aliases, truncate and position the destination at zero under their mutation
//! guard, and retain the guard through this call.
use std::{
    fs::File,
    io::{self, Seek},
};

pub fn copy_contents(source: &mut File, destination: &mut File) -> io::Result<()> {
    source.rewind()?;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // Keep both concrete File types so std can use copy_file_range,
        // sendfile or its fallback without an intermediate userspace buffer.
        io::copy(source, destination)?;
        Ok(())
    }
    #[cfg(target_vendor = "apple")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: both descriptors belong to live regular Files and remain
        // borrowed for the entire synchronous call. A null state asks libcopyfile
        // to manage its state. Never reopen either pathname in this operation.
        let result = unsafe {
            libc::fcopyfile(
                source.as_raw_fd(),
                destination.as_raw_fd(),
                std::ptr::null_mut(),
                libc::COPYFILE_DATA | libc::COPYFILE_METADATA,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
    {
        use std::io::{Read, Write};
        // Bounded handle-to-handle fallback. Path-based CopyFileEx/CopyFile2
        // cannot enrol the destination object before their destructive open.
        // Native Windows offload/performance acceptance remains a separate gate.
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = match source.read(&mut buffer) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if count == 0 {
                return Ok(());
            }
            destination.write_all(&buffer[..count])?;
        }
    }
}

#[cfg(test)]
mod tests;
