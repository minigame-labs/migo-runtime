// Copyright 2019-2021 the Deno authors. All rights reserved. MIT license.
use std::ffi::CStr;
use std::ffi::CString;
use std::sync::Mutex;
use std::vec::Vec;

use crate::platform::Platform;
use crate::support::MapFnFrom;
use crate::support::MapFnTo;
use crate::support::SharedRef;
use crate::support::ToCFn;
use crate::support::UnitType;
use crate::support::char;
use crate::support::int;

unsafe extern "C" {
  fn v8__V8__SetFlagsFromCommandLine(
    argc: *mut int,
    argv: *mut *mut char,
    usage: *const char,
  );
  fn v8__V8__SetFlagsFromString(flags: *const u8, length: usize);
  fn v8__V8__SetEntropySource(callback: EntropySource);
  fn v8__V8__GetVersion() -> *const char;
  fn v8__V8__InitializePlatform(platform: *mut Platform);
  fn v8__V8__Initialize();
  fn v8__V8__Dispose() -> bool;
  fn v8__V8__DisposePlatform();
  fn v8__V8__SetFatalErrorHandler(that: V8FatalErrorCallback);
  fn v8__V8__EnableWebAssemblyTrapHandler(use_v8_signal_handler: bool) -> bool;
}

pub type V8FatalErrorCallback = unsafe extern "C" fn(
  file: *const std::ffi::c_char,
  line: std::ffi::c_int,
  message: *const std::ffi::c_char,
);

/// EntropySource is used as a callback function when v8 needs a source
/// of entropy.
#[derive(Copy, Clone)]
#[repr(transparent)]
pub struct EntropySource(RawEntropySource);

pub trait IntoEntropySource:
  UnitType + Into<EntropySource> + FnOnce(&mut [u8]) -> bool
{
}

impl<F> IntoEntropySource for F where
  F: UnitType + Into<EntropySource> + FnOnce(&mut [u8]) -> bool
{
}

type RawEntropySource = unsafe extern "C" fn(*mut u8, usize) -> bool;

impl<F> From<F> for EntropySource
where
  F: UnitType + FnOnce(&mut [u8]) -> bool,
{
  fn from(_: F) -> Self {
    #[inline(always)]
    unsafe extern "C" fn adapter<F: IntoEntropySource>(
      buffer: *mut u8,
      length: usize,
    ) -> bool {
      let buffer = unsafe { std::slice::from_raw_parts_mut(buffer, length) };
      (F::get())(buffer)
    }

    Self(adapter::<F>)
  }
}

#[derive(Debug)]
enum GlobalState {
  Uninitialized,
  PlatformInitialized(SharedRef<Platform>),
  Initialized(SharedRef<Platform>),
  Disposed(#[allow(dead_code)] SharedRef<Platform>),
  PlatformShutdown,
}
use GlobalState::*;

static GLOBAL_STATE: Mutex<GlobalState> = Mutex::new(Uninitialized);

pub fn assert_initialized() {
  let global_state_guard = GLOBAL_STATE.lock().unwrap();
  match *global_state_guard {
    Initialized(_) => {}
    _ => panic!("Invalid global state"),
  };
}

/// Pass the command line arguments to v8.
/// The first element of args (which usually corresponds to the binary name) is
/// ignored.
/// Returns a vector of command line arguments that V8 did not understand.
/// TODO: Check whether this is safe to do after globally initializing v8.
pub fn set_flags_from_command_line(args: Vec<String>) -> Vec<String> {
  set_flags_from_command_line_with_usage(args, None)
}

/// The example code below is here to avoid the V8 usage string and options
/// that are printed to stdout by this function. Placing this here instead of
/// in a test allows the output to be suppressed.
///
/// # Examples
///
/// ```
///     let r = v8::V8::set_flags_from_command_line_with_usage(
///       vec!["binaryname".to_string(), "--help".to_string()],
///       Some("Usage: binaryname --startup-src=file\n\n"),
///     );
///     assert_eq!(r, vec!["binaryname".to_string()]);
/// ```
pub fn set_flags_from_command_line_with_usage(
  args: Vec<String>,
  usage: Option<&str>,
) -> Vec<String> {
  // deno_set_v8_flags(int* argc, char** argv) mutates argc and argv to remove
  // flags that v8 understands.

  // Make a new array, that can be modified by V8::SetFlagsFromCommandLine(),
  // containing mutable raw pointers to the individual command line args.
  let mut raw_argv = args
    .iter()
    .map(|arg| CString::new(arg.as_str()).unwrap().into_bytes_with_nul())
    .collect::<Vec<_>>();
  let mut c_argv = raw_argv
    .iter_mut()
    .map(|arg| arg.as_mut_ptr() as *mut char)
    .collect::<Vec<_>>();

  // Store the length of the c_argv array in a local variable. We'll pass
  // a pointer to this local variable to deno_set_v8_flags(), which then
  // updates its value.
  let mut c_argv_len = c_argv.len() as int;
  // Let v8 parse the arguments it recognizes and remove them from c_argv.
  let c_usage = match usage {
    Some(str) => CString::new(str).unwrap().into_raw() as *const char,
    None => std::ptr::null(),
  };
  unsafe {
    v8__V8__SetFlagsFromCommandLine(
      &mut c_argv_len,
      c_argv.as_mut_ptr(),
      c_usage,
    );
  };
  // If c_argv_len was updated we have to change the length of c_argv to match.
  c_argv.truncate(c_argv_len as usize);
  // Copy the modified arguments list into a proper rust vec and return it.
  c_argv
    .iter()
    .map(|ptr| unsafe {
      let cstr = CStr::from_ptr(*ptr as *const char);
      let slice = cstr.to_str().unwrap();
      slice.to_string()
    })
    .collect()
}

/// Sets V8 flags from a string.
pub fn set_flags_from_string(flags: &str) {
  unsafe {
    v8__V8__SetFlagsFromString(flags.as_ptr(), flags.len());
  }
}

/// Allows the host application to provide a callback which can be used
/// as a source of entropy for random number generators.
pub fn set_entropy_source(
  callback: impl UnitType + Into<EntropySource> + FnOnce(&mut [u8]) -> bool,
) {
  unsafe { v8__V8__SetEntropySource(callback.into()) };
}

/// Get the version string.
pub fn get_version() -> &'static str {
  let version = unsafe { v8__V8__GetVersion() };
  let c_str = unsafe { CStr::from_ptr(version) };
  c_str.to_str().unwrap()
}

// MIGO: on Windows rusty_v8 is a DLL, and a DLL must have every symbol it
// references resolved at link time. The callbacks V8 makes back into Rust
// therefore cannot be plain imports; the DLL holds them in a table this fills in
// before V8 is touched. Everywhere else rusty_v8 is a static library, the final
// link supplies them directly, and none of this is compiled.
#[cfg(target_os = "windows")]
mod host_callbacks {
  use std::ffi::c_void;
  use std::os::raw::c_char;

  unsafe extern "C" {
    fn v8__register_host_callback(name: *const c_char, f: *const c_void) -> i32;
    fn v8__host_callbacks_ready() -> i32;
  }

  /// Binds each callback under the name the DLL knows it by.
  ///
  /// The name is derived from the function identifier with `stringify!` rather
  /// than written out beside it. `#[no_mangle]` makes the exported symbol name
  /// identical to the Rust identifier, so the two cannot drift: a mistyped
  /// entry fails to resolve as a Rust path and does not compile, instead of
  /// registering under a name nothing looks up.
  macro_rules! register_all {
    ($($module:ident :: $func:ident),* $(,)?) => {
      $({
        const NAME: &str = concat!(stringify!($func), "\0");
        // SAFETY: NAME is NUL-terminated by construction and the function is a
        // `#[no_mangle] extern "C"` item, so its address is a valid callback.
        let accepted = unsafe {
          v8__register_host_callback(
            NAME.as_ptr() as *const c_char,
            crate::$module::$func as *const c_void,
          )
        };
        assert!(
          accepted == 1,
          "the rusty_v8 DLL does not know the host callback {}",
          stringify!($func),
        );
      })*
    };
  }

  pub(crate) fn register() {
    register_all!(
      crdtp::crdtp__FrontendChannel__BASE__sendProtocolResponse,
      crdtp::crdtp__FrontendChannel__BASE__sendProtocolNotification,
      crdtp::crdtp__FrontendChannel__BASE__fallThrough,
      crdtp::crdtp__FrontendChannel__BASE__flushProtocolNotifications,
      crdtp::crdtp__DomainDispatcher__BASE__Dispatch,
      crdtp::crdtp__DomainDispatcher__BASE__Drop,
      platform::v8__Platform__CustomPlatform__BASE__PostTask,
      platform::v8__Platform__CustomPlatform__BASE__PostNonNestableTask,
      platform::v8__Platform__CustomPlatform__BASE__PostDelayedTask,
      platform::v8__Platform__CustomPlatform__BASE__PostNonNestableDelayedTask,
      platform::v8__Platform__CustomPlatform__BASE__PostIdleTask,
      platform::v8__Platform__CustomPlatform__BASE__DROP,
      value_serializer::v8__ValueSerializer__Delegate__ThrowDataCloneError,
      value_serializer::v8__ValueSerializer__Delegate__HasCustomHostObject,
      value_serializer::v8__ValueSerializer__Delegate__IsHostObject,
      value_serializer::v8__ValueSerializer__Delegate__WriteHostObject,
      value_serializer::v8__ValueSerializer__Delegate__GetSharedArrayBufferId,
      value_serializer::v8__ValueSerializer__Delegate__GetWasmModuleTransferId,
      value_serializer::v8__ValueSerializer__Delegate__ReallocateBufferMemory,
      value_serializer::v8__ValueSerializer__Delegate__FreeBufferMemory,
      value_deserializer::v8__ValueDeserializer__Delegate__ReadHostObject,
      value_deserializer::v8__ValueDeserializer__Delegate__GetSharedArrayBufferFromId,
      value_deserializer::v8__ValueDeserializer__Delegate__GetWasmModuleFromId,
      cppgc::rusty_v8_RustObj_trace,
      cppgc::rusty_v8_RustObj_get_name,
      cppgc::rusty_v8_RustObj_drop,
      inspector::v8_inspector__V8Inspector__Channel__BASE__sendResponse,
      inspector::v8_inspector__V8Inspector__Channel__BASE__sendNotification,
      inspector::v8_inspector__V8Inspector__Channel__BASE__flushProtocolNotifications,
      inspector::v8_inspector__V8InspectorClient__BASE__generateUniqueId,
      inspector::v8_inspector__V8InspectorClient__BASE__runMessageLoopOnPause,
      inspector::v8_inspector__V8InspectorClient__BASE__quitMessageLoopOnPause,
      inspector::v8_inspector__V8InspectorClient__BASE__runIfWaitingForDebugger,
      inspector::v8_inspector__V8InspectorClient__BASE__consoleAPIMessage,
      inspector::v8_inspector__V8InspectorClient__BASE__ensureDefaultContextInGroup,
      inspector::v8_inspector__V8InspectorClient__BASE__resourceNameToUrl,
    );

    // Catches the other direction: a callback the DLL expects that this list
    // does not supply. Without it such a slot stays null and faults later, at
    // the first serialization or inspector message rather than here.
    // SAFETY: plain query, no arguments.
    assert!(
      unsafe { v8__host_callbacks_ready() } == 1,
      "the rusty_v8 DLL expects host callbacks this build does not register",
    );
  }
}

/// Binds the Windows DLL's host-callback table. A no-op elsewhere.
pub(crate) fn register_host_callbacks_once() {
  #[cfg(target_os = "windows")]
  {
    use std::sync::Once;
    static REGISTERED: Once = Once::new();
    REGISTERED.call_once(host_callbacks::register);
  }
}

/// Sets the v8::Platform to use. This should be invoked before V8 is
/// initialized.
pub fn initialize_platform(platform: SharedRef<Platform>) {
  // Before any call reaches V8: this is the documented first entry point, so
  // binding here guarantees the table is filled before anything can call back.
  register_host_callbacks_once();

  let mut global_state_guard = GLOBAL_STATE.lock().unwrap();
  *global_state_guard = match *global_state_guard {
    Uninitialized => PlatformInitialized(platform.clone()),
    _ => panic!("Invalid global state"),
  };

  {
    unsafe {
      v8__V8__InitializePlatform(&*platform as *const Platform as *mut _);
    };
  }
}

/// Initializes V8. This function needs to be called before the first Isolate
/// is created. It always returns true.
pub fn initialize() {
  let mut global_state_guard = GLOBAL_STATE.lock().unwrap();
  *global_state_guard = match *global_state_guard {
    PlatformInitialized(ref platform) => Initialized(platform.clone()),
    _ => panic!("Invalid global state"),
  };
  unsafe { v8__V8__Initialize() }
}

/// Sets the v8::Platform to use. This should be invoked before V8 is
/// initialized.
pub fn get_current_platform() -> SharedRef<Platform> {
  let global_state_guard = GLOBAL_STATE.lock().unwrap();
  match *global_state_guard {
    Initialized(ref platform) => platform.clone(),
    _ => panic!("Invalid global state"),
  }
}

/// Releases any resources used by v8 and stops any utility threads
/// that may be running.  Note that disposing v8 is permanent, it
/// cannot be reinitialized.
///
/// It should generally not be necessary to dispose v8 before exiting
/// a process, this should happen automatically.  It is only necessary
/// to use if the process needs the resources taken up by v8.
///
/// # Safety
///
/// Calling this function before completely disposing all isolates will lead
/// to a crash.
pub unsafe fn dispose() -> bool {
  let mut global_state_guard = GLOBAL_STATE.lock().unwrap();
  *global_state_guard = match *global_state_guard {
    Initialized(ref platform) => Disposed(platform.clone()),
    _ => panic!("Invalid global state"),
  };
  assert!(unsafe { v8__V8__Dispose() });
  true
}

/// Clears all references to the v8::Platform. This should be invoked after
/// V8 was disposed. If it is called if V8 is not disposed, it will panic.
pub fn dispose_platform() {
  let mut global_state_guard = GLOBAL_STATE.lock().unwrap();
  // First check that the global state is disposed. If it is we dispose the
  // platform. We then drop the platform.
  *global_state_guard = match *global_state_guard {
    Disposed(_) => {
      unsafe { v8__V8__DisposePlatform() };
      PlatformShutdown
    }
    _ => panic!("Invalid global state"),
  };
}

impl<F> MapFnFrom<F> for V8FatalErrorCallback
where
  F: UnitType + Fn(&str, i32, &str),
{
  fn mapping() -> Self {
    let f = |file, line, message| unsafe {
      let file = std::ffi::CStr::from_ptr(file).to_str().unwrap_or_default();
      let message = std::ffi::CStr::from_ptr(message)
        .to_str()
        .unwrap_or_default();
      (F::get())(file, line, message);
    };
    f.to_c_fn()
  }
}

/// Set the callback to invoke in the case of CHECK failures or fatal
/// errors. This is distinct from Isolate::SetFatalErrorHandler, which
/// is invoked in response to API usage failures.
pub fn set_fatal_error_handler(that: impl MapFnTo<V8FatalErrorCallback>) {
  unsafe {
    v8__V8__SetFatalErrorHandler(that.map_fn_to());
  }
}

/// Activate trap-based bounds checking for WebAssembly.
///
/// If `use_v8_signal_handler` is true, V8 installs its own signal handler.
/// Otherwise, V8 relies on the embedder to invoke its trap handler.
pub fn enable_web_assembly_trap_handler(use_v8_signal_handler: bool) -> bool {
  unsafe { v8__V8__EnableWebAssemblyTrapHandler(use_v8_signal_handler) }
}

#[cfg(test)]
mod test_sandbox_use {
  // __IsSandboxEnabled is used for testing that sandbox is actually
  // enabled and is not a stable API
  unsafe extern "C" {
    fn v8__V8__IsSandboxEnabled() -> bool;
  }

  fn is_sandboxed() -> bool {
    unsafe { v8__V8__IsSandboxEnabled() }
  }

  #[test]
  #[cfg(feature = "v8_enable_sandbox")]
  fn test_sandbox_on() {
    assert!(is_sandboxed());
  }

  #[test]
  #[cfg(not(feature = "v8_enable_sandbox"))]
  fn test_sandbox_off() {
    assert!(!is_sandboxed());
  }
}
