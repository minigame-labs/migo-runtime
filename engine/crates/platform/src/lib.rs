#[cfg(target_os = "android")]
pub mod android;
#[cfg(any(target_os = "android", test))]
mod android_permission_gate;

#[cfg(any(target_os = "android", test))]
#[path = "android/frame_rate.rs"]
mod android_frame_rate;

#[cfg(any(target_os = "android", test))]
#[path = "android/gamepad_buffer.rs"]
mod android_gamepad_buffer;

#[cfg(any(target_os = "android", test))]
mod host_owners;

#[cfg(any(target_os = "android", test))]
mod jni_method_id;
// Compiled on every host, not only Android: the profile contract's own tests
// are what keep the JNI surface and the profile features in step, and they have
// to run somewhere with a CI runner. The external-frame product is the
// exception -- it is the Apple lane, it registers no JNI methods, and compiling
// a contract with no surface to describe would be dead code that reads as
// coverage.
#[cfg(not(feature = "external-frames"))]
#[path = "android/jni/profile_contract.rs"]
pub(crate) mod jni_profile_contract;
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
pub mod linux;

// `target_vendor` rather than `target_os`, unlike every other platform here.
// macOS and iOS differ in exactly one line of this module -- the name of the
// file ANGLE ships in -- so a `target_os` pair would be two copies of one
// presenter, and a third Apple platform would silently reach neither.
//
// `test` as well, and that is the load-bearing half: an Apple-only module is a
// module no pull request executes, because every gate in this repository runs on
// Linux. Under `cfg(test)` the surface identity, factory refusal and provider
// pairing logic runs on every Linux run instead. Nothing here loads EGL at
// construction time, so there is nothing Apple-specific to have available.
//
// `feature = "test-support"` for the same coverage one crate further out.
// `cfg(test)` does not cross a crate boundary, so without it the C ABI's own
// Apple module -- eight match arms over a platform payload enum -- could not be
// compiled anywhere but on a Mac. `migo-capi` already enables this feature from
// its `[dev-dependencies]` for the X11 arm, which resolver 2 keeps out of every
// shipped build.
#[cfg(any(target_vendor = "apple", test, feature = "test-support"))]
pub mod apple;

// OpenHarmony reports target_os = "linux" with target_env = "ohos", so it must
// be selected on the env rather than the OS -- the desktop Linux module above
// excludes it for the same reason.
#[cfg(all(target_os = "linux", target_env = "ohos"))]
pub mod ohos;

// `test` and `feature = "test-support"` for the reason spelled out above the
// Apple module, and it applies here word for word: a Windows-only module is a
// module no pull request executes, because every gate in this repository runs on
// Linux. Nothing in here is Windows-specific to the compiler -- the whole module
// imports only std, `graphics::egl_platform`, `khronos_egl`, `shared` and
// `migo_core`, and an HWND is carried as a `NonNull<c_void>` -- so the surface
// identity and factory-refusal logic compiles and runs on Linux exactly as the
// Apple presenter's does.
//
// What that is worth, measured rather than assumed: the Windows compile lane
// builds this module and runs nothing in it, so its twelve tests had never
// executed anywhere; they pass. `same_native_surface`'s `_ => false` arm was in
// the same state the Apple one was found in -- flipping it to `true` left all
// six presenter tests green -- and is now covered by
// `offscreen_and_window_targets_are_never_the_same_surface`.
#[cfg(any(target_os = "windows", test, feature = "test-support"))]
pub mod windows;
#[cfg(all(feature = "profile-full", feature = "profile-slim"))]
compile_error!("profile-full and profile-slim are mutually exclusive");
// The other half, so "which profile is this" is a total question rather than a
// mostly-true one. `active_methods` selects the JNI surface with a chain of
// `#[cfg(feature)]` attributes and the profile rule states it declaratively;
// tying the two together needs a build to have exactly one answer, not zero.
// Every dependent already forwards a profile through its own `default`, so this
// only rejects a bare `--no-default-features`, which is not a product.
#[cfg(not(any(
    feature = "profile-full",
    feature = "profile-slim",
    feature = "external-frames"
)))]
compile_error!("exactly one of profile-full, profile-slim and external-frames must be enabled");
// The external-frame product has no embedded engine, so it cannot also carry a
// profile that describes one. Rejected here rather than resolved by precedence,
// for the same reason `migo-core` rejects the pair: failing here says which
// flag to drop, and failing in a dependency-closure gate says only that V8 is
// present.
#[cfg(all(
    feature = "external-frames",
    any(feature = "profile-full", feature = "profile-slim")
))]
compile_error!(
    "external-frames cannot be combined with an embedded profile: the lane exists to \
     prove no JavaScript engine is linked"
);
#[cfg(all(feature = "worker-snapshot", not(feature = "profile-full")))]
compile_error!("worker-snapshot requires profile-full");
#[cfg(all(
    feature = "profile-slim",
    any(
        feature = "api-sensors",
        feature = "api-media",
        feature = "api-connectivity",
        feature = "api-commerce",
        feature = "api-system"
    )
))]
compile_error!("profile-slim is exact and cannot be combined with optional API groups");
