//! Optional LLVM RTSan scopes for explicitly instrumented native test binaries.
//! GlobalAlloc counters cannot see std::alloc::System or C allocators. Resolve
//! and initialize this runtime on the control thread, then enter on the actual
//! OS-created callback thread. An ordinary binary fails closed when asked to
//! run this gate; there is no successful uninstrumented fallback.
use std::io;

type Hook = unsafe extern "C" fn();

#[derive(Clone, Copy)]
pub struct RealtimeProbe {
    enter: Hook,
    exit: Hook,
}

impl RealtimeProbe {
    /// Mark the exact native callback interval for an independent syscall
    /// trace. Both writes happen outside the RT scope. A syscall suppression
    /// alone cannot distinguish FUTEX_WAKE from a blocking FUTEX_WAIT.
    #[cfg(target_os = "linux")]
    pub fn run_traced<R>(&self, body: impl FnOnce() -> R) -> R {
        fn marker(bytes: &[u8]) {
            // SAFETY: a static borrowed slice is valid for this synchronous
            // write; stderr is owned by the test runner for its full lifetime.
            let written = unsafe { libc::write(2, bytes.as_ptr().cast(), bytes.len()) };
            assert_eq!(written, bytes.len() as isize, "RT trace marker write");
        }
        marker(b"MIGO_RT_BEGIN\n");
        let result = self.run(body);
        marker(b"MIGO_RT_END\n");
        result
    }

    pub fn load() -> io::Result<Self> {
        fn hook(name: &std::ffi::CStr) -> io::Result<Hook> {
            // SAFETY: look up a process-lifetime C interface with the exact
            // fixed void(void) ABI. This runs before the measured callback.
            let pointer = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
            if pointer.is_null() {
                return Err(io::Error::other(
                    "LLVM RTSan is not linked; run the explicit native realtime contract",
                ));
            }
            Ok(unsafe { std::mem::transmute::<*mut libc::c_void, Hook>(pointer) })
        }
        let initialize = hook(c"__rtsan_ensure_initialized")?;
        let enter = hook(c"__rtsan_realtime_enter")?;
        let exit = hook(c"__rtsan_realtime_exit")?;
        unsafe { initialize() };
        Ok(Self { enter, exit })
    }

    pub fn run<R>(&self, body: impl FnOnce() -> R) -> R {
        struct Scope(Hook, std::marker::PhantomData<std::rc::Rc<()>>);
        impl Drop for Scope {
            fn drop(&mut self) {
                // SAFETY: this guard cannot migrate and balances the entry.
                unsafe { (self.0)() };
            }
        }
        // RTSan initializes this OS thread's own context before marking it RT.
        unsafe { (self.enter)() };
        let _scope = Scope(self.exit, std::marker::PhantomData);
        body()
    }
}
