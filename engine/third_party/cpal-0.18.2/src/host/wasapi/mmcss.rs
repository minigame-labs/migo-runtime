//! A scoped WASAPI worker registration using the Windows bindings CPAL already
//! depends on. Refusal is nonfatal; no TIME_CRITICAL fallback is requested.
use windows::{
    core::w,
    Win32::{Foundation::HANDLE, System::Threading},
};

use crate::{Error, ErrorKind};

#[path = "mmcss_scope.rs"]
mod scope;

pub(super) struct MmcssGuard {
    _registration: scope::Registration<HANDLE>,
}

fn release(handle: HANDLE) {
    // SAFETY: the unique registration came from this worker, and the !Send/
    // !Sync scope cannot leave that thread. The API consumes its registration,
    // not a kernel handle for CloseHandle. Drop must not unwind on OS refusal.
    let result = unsafe { Threading::AvRevertMmThreadCharacteristics(handle) };
    #[cfg(test)]
    RESTORED.with(|state| state.set(Some(result.is_ok())));
    let _ = result;
}

pub(super) fn register() -> Result<MmcssGuard, Error> {
    let registration = scope::Registration::acquire(
        || {
            let mut task_index = 0;
            // SAFETY: a static, null-terminated Windows task name and a live
            // writable index. Registration and release occur on this thread.
            unsafe { Threading::AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) }
                .map_err(|_| {
                    Error::with_message(ErrorKind::RealtimeDenied, "MMCSS registration denied")
                })
        },
        |handle| {
            // SAFETY: the handle belongs to the live, same-thread scope. An
            // error rolls back registration before it reaches the caller.
            unsafe { Threading::AvSetMmThreadPriority(*handle, Threading::AVRT_PRIORITY_HIGH) }
                .map_err(|_| {
                    Error::with_message(ErrorKind::RealtimeDenied, "MMCSS high priority denied")
                })
        },
        release,
    )?;
    Ok(MmcssGuard {
        _registration: registration,
    })
}

#[cfg(test)]
thread_local! {
    static RESTORED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_windows_registration_is_reverted_or_denied_without_a_fatal_error() {
        RESTORED.with(|state| state.set(None));
        match register() {
            Ok(guard) => {
                assert_eq!(RESTORED.with(|state| state.get()), None);
                drop(guard);
                assert_eq!(RESTORED.with(|state| state.get()), Some(true));
                println!("MMCSS_NATIVE registered-high-and-reverted-on-owner");
            }
            Err(error) => {
                assert_eq!(error.kind(), ErrorKind::RealtimeDenied);
                // Failed priority configuration may already have reverted an
                // acquired registration; failed acquisition has none to undo.
                assert_ne!(RESTORED.with(|state| state.get()), Some(false));
                println!("MMCSS_NATIVE denied-fallback-normal; no native-promotion proof");
            }
        }
    }
}
