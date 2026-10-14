//! The device's physical memory: what a resource ceiling that follows the device is a share of.
//!
//! A ceiling written as a fixed number is wrong on most devices: generous on the smallest phone, and on everything
//! else a refusal of work the device could do. Scaling it with physical memory keeps one rule meaningful from a
//! 2 GiB phone to a desktop.

use std::sync::OnceLock;

/// The device's total physical memory in bytes, or `None` where the system does not say. Asked once per process.
pub fn physical_memory_bytes() -> Option<u64> {
    static PHYSICAL: OnceLock<Option<u64>> = OnceLock::new();
    *PHYSICAL.get_or_init(query)
}

#[cfg(unix)]
fn query() -> Option<u64> {
    // SAFETY: sysconf reads a system value and has no preconditions.
    let (pages, page_size) = unsafe {
        (
            libc::sysconf(libc::_SC_PHYS_PAGES),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    let pages = u64::try_from(pages).ok().filter(|pages| *pages > 0)?;
    let page_size = u64::try_from(page_size).ok().filter(|size| *size > 0)?;
    pages.checked_mul(page_size)
}

#[cfg(windows)]
fn query() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: MEMORYSTATUSEX is plain data, and GlobalMemoryStatusEx writes it once its length is set, as documented.
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    let answered = unsafe { GlobalMemoryStatusEx(&mut status) } != 0;
    (answered && status.ullTotalPhys > 0).then_some(status.ullTotalPhys)
}

#[cfg(not(any(unix, windows)))]
fn query() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every platform Migo ships on says how much memory it has, and no device it runs on has less than 512 MiB.
    #[test]
    fn the_device_says_how_much_memory_it_has() {
        let bytes = physical_memory_bytes().expect("the system reports its physical memory");
        assert!(
            bytes >= 512 << 20,
            "{bytes} bytes is less than any device Migo runs on"
        );
        assert_eq!(
            physical_memory_bytes(),
            Some(bytes),
            "asked once, the same answer after"
        );
    }
}
