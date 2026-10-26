//! Run a probe on an OS-created thread, without std::thread::Builder's eager
//! Rust thread registration. Device callbacks often enter Rust the same way.
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

struct Context<F, R> {
    body: Option<F>,
    result: Option<std::thread::Result<R>>,
}

impl<F: FnOnce() -> R, R> Context<F, R> {
    fn run(&mut self) {
        // No Rust unwind crosses the native thread ABI. Propagate it after
        // join on the caller so a failed assertion still fails its Rust test.
        self.result = Some(catch_unwind(AssertUnwindSafe(|| {
            self.body.take().expect("native probe entry runs once")()
        })));
    }
}

/// Execute on an OS-created thread and join before returning. `body` and its
/// result are owned/Send/'static; an exceptional native join failure can retain
/// the context safely rather than freeing memory a live thread may still use.
///
/// This helper does not install an allocator. The caller's test binary must
/// install CountingAllocator and observe counters inside `body`.
pub fn on_native_thread<F, R>(body: F) -> io::Result<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let context = Box::into_raw(Box::new(Context {
        body: Some(body),
        result: None,
    }));
    // SAFETY: the allocation remains owned by this launch until native join;
    // on a failed join it is deliberately retained. F and R satisfy Send.
    unsafe { launch(context) }?;
    // SAFETY: launch returns Ok only after join. No thread can access context
    // again, so the original allocation can be reclaimed and read exclusively.
    let mut context = unsafe { Box::from_raw(context) };
    match context
        .result
        .take()
        .expect("native probe completed its entry")
    {
        Ok(value) => Ok(value),
        Err(panic) => resume_unwind(panic),
    }
}

#[cfg(unix)]
unsafe fn launch<F, R>(context: *mut Context<F, R>) -> io::Result<()>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    extern "C" fn entry<F: FnOnce() -> R, R>(raw: *mut libc::c_void) -> *mut libc::c_void {
        // SAFETY: pthread_create receives our live Context and invokes once.
        unsafe { (&mut *raw.cast::<Context<F, R>>()).run() };
        std::ptr::null_mut()
    }
    let mut thread = std::mem::MaybeUninit::<libc::pthread_t>::uninit();
    // SAFETY: default joinable attributes, initialized context and matching ABI.
    let error = unsafe {
        libc::pthread_create(
            thread.as_mut_ptr(),
            std::ptr::null(),
            entry::<F, R>,
            context.cast(),
        )
    };
    if error != 0 {
        // SAFETY: no child was created on error, so ownership never left us.
        drop(unsafe { Box::from_raw(context) });
        return Err(io::Error::from_raw_os_error(error));
    }
    // SAFETY: a successfully created, joinable thread; no other joiner exists.
    let error = unsafe { libc::pthread_join(thread.assume_init(), std::ptr::null_mut()) };
    if error != 0 {
        // Keep the allocation alive: a failed join proves no completion.
        return Err(io::Error::from_raw_os_error(error));
    }
    Ok(())
}

#[cfg(windows)]
unsafe fn launch<F, R>(context: *mut Context<F, R>) -> io::Result<()>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::Threading::{CreateThread, INFINITE, WaitForSingleObject},
    };
    unsafe extern "system" fn entry<F: FnOnce() -> R, R>(raw: *mut core::ffi::c_void) -> u32 {
        // SAFETY: CreateThread receives our live Context and invokes once.
        unsafe { (&mut *raw.cast::<Context<F, R>>()).run() };
        0
    }
    // SAFETY: default security/stack, valid context, no borrowed input.
    let thread = unsafe {
        CreateThread(
            std::ptr::null(),
            0,
            Some(entry::<F, R>),
            context.cast(),
            0,
            std::ptr::null_mut(),
        )
    };
    if thread.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: no child was created, so reclaim the original allocation.
        drop(unsafe { Box::from_raw(context) });
        return Err(error);
    }
    // SAFETY: wait on this function's valid thread handle, then release it.
    let result = unsafe { WaitForSingleObject(thread, INFINITE) };
    let error = if result == WAIT_OBJECT_0 {
        None
    } else {
        Some(io::Error::last_os_error())
    };
    unsafe { CloseHandle(thread) };
    if let Some(error) = error {
        return Err(error);
    }
    Ok(())
}
