//! Linux system-EGL presenter boundary.
//!
//! Mirrors `platform/android/presenter.rs` for the `x86_64-unknown-linux-gnu`
//! support profile. It provides a headless pbuffer path plus host-owned X11 and
//! Wayland onscreen targets through the same injection contract
//! (`EglProvider` / `EglSurfaceFactory` / `GraphicsPlatform`).
//!
//! NOTE: the chosen `egl::Config` must advertise `EGL_PBUFFER_BIT` in its
//! surface type for `create_pbuffer_surface` to succeed. The graphics EGL
//! manager owns config selection; onscreen-only configs will reject the
//! pbuffer path.

use std::{
    any::Any,
    ffi::{c_ulong, c_void},
    ptr::NonNull,
    sync::Arc,
};

use graphics::egl_platform::{
    EglConcurrency, EglInstance, EglProvider, EglSurfaceFactory, GraphicsBackendId,
    GraphicsPlatform, PlatformIdentity, PreparedEglSurface, PreparedEglSurfaceRef,
};
use khronos_egl as egl;
use shared::{
    error::{EngineError, EngineResult, ErrorCode},
    surface::{Surface, SurfaceRef},
};

use super::{egl_fallback, x11_connection::X11RenderConnection};

/// Re-exported so a test-support consumer needs one import path for the X11
/// context and the topology it is opened against.
#[cfg(any(test, feature = "test-support"))]
pub use super::x11_connection::X11TestServers;

/// System EGL shared object on glibc Linux. The unversioned `libEGL.so` symlink
/// ships only with `-dev` packages, so the runtime `.so.1` is loaded directly.
const LINUX_EGL_LIBRARY: &str = "libEGL.so.1";

/// X11 platform token, identical in `EGL_EXT_platform_x11` and
/// `EGL_KHR_platform_x11`. Not re-exported by `khronos-egl`, which carries core
/// enums only.
const EGL_PLATFORM_X11_EXT: egl::Enum = 0x31D5;
const EGL_PLATFORM_WAYLAND_EXT: egl::Enum = 0x31D8;
/// `EGL_MESA_platform_surfaceless`: a display that needs no window system.
const EGL_PLATFORM_SURFACELESS_MESA: egl::Enum = 0x31DD;

/// The Wayland EGL glue, loaded at run time rather than linked.
///
/// EGL cannot make a surface out of a `wl_surface` directly: it needs a
/// `wl_egl_window`, and only this library can build one. Resolving it at run
/// time keeps the SDK free of a build-time Wayland dependency, exactly as the
/// X11 path takes an opaque window token and links no X library — a host that
/// never attaches a Wayland surface never loads this.
const LINUX_WAYLAND_EGL_LIBRARY: &str = "libwayland-egl.so.1";

/// Platform-display entry points from EGL 1.5 / `EGL_EXT_platform_base`.
///
/// The engine's [`EglInstance`] is typed to **EGL 1.4** because that is the
/// floor the Android support profile can rely on: `load_required_from` resolves
/// every symbol of the requested version up front, so typing it to 1.5 would
/// refuse to load on a 1.4-only driver and break the shipping platform. The
/// 1.5 core calls are therefore unavailable through the instance, and the
/// platform entry points — resolved at runtime, exactly as GLFW/SDL/Qt do —
/// are how a specific native platform is selected without touching that floor.
///
/// When the extension is absent the caller falls back to the legacy calls,
/// which let the driver infer the platform from the pointer.
mod platform_ext {
    use super::*;

    pub(super) type GetPlatformDisplay =
        unsafe extern "system" fn(egl::Enum, *mut c_void, *const egl::Int) -> *mut c_void;

    pub(super) type CreatePlatformWindowSurface = unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *const egl::Int,
    ) -> *mut c_void;

    /// Resolve an extension entry point, or `None` when the driver lacks it.
    ///
    /// # Safety
    /// The caller must instantiate `F` with the signature the EGL specification
    /// gives for `name`.
    pub(super) unsafe fn entry_point<F: Copy>(egl: &EglInstance, name: &str) -> Option<F> {
        // A function pointer is pointer-sized; the transmute below reinterprets
        // it as the specified prototype, which is the documented way to use
        // eglGetProcAddress.
        const _: () = assert!(size_of::<extern "system" fn()>() == size_of::<*const c_void>());
        egl.get_proc_address(name)
            .map(|pointer| unsafe { std::mem::transmute_copy::<_, F>(&pointer) })
    }

    pub(super) fn first_resolved<F: Copy>(
        names: &[&str],
        mut resolve: impl FnMut(&str) -> Option<F>,
    ) -> Option<F> {
        names.iter().find_map(|name| resolve(name))
    }

    /// Prefer EGL 1.5 core entry points, then their EXT aliases. Both use the
    /// same calling convention for the null attribute lists used here.
    pub(super) unsafe fn first_entry_point<F: Copy>(
        egl: &EglInstance,
        names: &[&str],
    ) -> Option<F> {
        first_resolved(names, |name| unsafe { entry_point(egl, name) })
    }
}

const GET_PLATFORM_DISPLAY_NAMES: [&str; 2] = ["eglGetPlatformDisplay", "eglGetPlatformDisplayEXT"];
const CREATE_PLATFORM_WINDOW_SURFACE_NAMES: [&str; 2] = [
    "eglCreatePlatformWindowSurface",
    "eglCreatePlatformWindowSurfaceEXT",
];

fn native_platform_display(
    egl: &EglInstance,
    platform: egl::Enum,
    native_display: NonNull<c_void>,
    target: &str,
) -> EngineResult<egl::Display> {
    let platform_entry = unsafe {
        platform_ext::first_entry_point::<platform_ext::GetPlatformDisplay>(
            egl,
            &GET_PLATFORM_DISPLAY_NAMES,
        )
    };
    let entry_available = platform_entry.is_some();
    let mut platform_error = None;
    let mut legacy_error = None;
    let raw = egl_fallback::preferred_or_fallback(
        platform_entry,
        |get_platform_display| {
            // SAFETY: signature per EGL 1.5 / EXT_platform_base. The native
            // display is host-owned and a null attribute list is valid for
            // both the core EGLAttrib and EXT EGLint forms.
            let raw = unsafe {
                get_platform_display(platform, native_display.as_ptr(), std::ptr::null())
            };
            NonNull::new(raw).or_else(|| {
                // A global loader may expose a non-null stub for an unsupported
                // platform. Consume its error before trying the EGL 1.4 native
                // binding so diagnostics and the next call remain well scoped.
                platform_error = egl.get_error();
                None
            })
        },
        || {
            let display = unsafe { egl.get_display(native_display.as_ptr()) };
            display
                .map(|display| {
                    // EGL handle wrappers are guaranteed non-null on `Some`.
                    NonNull::new(display.as_ptr()).expect("EGL Display invariant")
                })
                .or_else(|| {
                    legacy_error = egl.get_error();
                    None
                })
        },
    );

    raw.map(|raw| {
        // SAFETY: non-null and produced by EGL itself.
        unsafe { egl::Display::from_ptr(raw.as_ptr()) }
    })
    .ok_or_else(|| {
        EngineError::new(ErrorCode::RenderInitializeError)
            .with_msg(format!("Linux {target} EGL display unavailable"))
            .with_detail(format!(
                "platform_entry_available={entry_available}, platform_error={platform_error:?}, legacy_error={legacy_error:?}"
            ))
    })
}

fn create_native_window_surface(
    egl: &EglInstance,
    display: egl::Display,
    config: egl::Config,
    platform_native_window: *mut c_void,
    legacy_native_window: *mut c_void,
    target: &str,
) -> EngineResult<egl::Surface> {
    let platform_entry = unsafe {
        platform_ext::first_entry_point::<platform_ext::CreatePlatformWindowSurface>(
            egl,
            &CREATE_PLATFORM_WINDOW_SURFACE_NAMES,
        )
    };
    let entry_available = platform_entry.is_some();
    let mut platform_error = None;
    let mut legacy_error = None;
    let surface = egl_fallback::preferred_or_fallback(
        platform_entry,
        |create_platform_window_surface| {
            // SAFETY: signature per EGL 1.5 / EXT_platform_base. The caller
            // supplies the platform-specific native-window representation;
            // null attributes are valid for both entry-point variants.
            let raw = unsafe {
                create_platform_window_surface(
                    display.as_ptr(),
                    config.as_ptr(),
                    platform_native_window,
                    std::ptr::null(),
                )
            };
            NonNull::new(raw)
                .map(|raw| {
                    // SAFETY: non-null and produced by EGL itself.
                    unsafe { egl::Surface::from_ptr(raw.as_ptr()) }
                })
                .or_else(|| {
                    platform_error = egl.get_error();
                    None
                })
        },
        || {
            match unsafe { egl.create_window_surface(display, config, legacy_native_window, None) }
            {
                Ok(surface) => Some(surface),
                Err(error) => {
                    // The safe wrapper has already consumed eglGetError.
                    legacy_error = Some(error);
                    None
                }
            }
        },
    );

    surface.ok_or_else(|| {
        EngineError::new(ErrorCode::RenderBackendError)
            .with_msg(format!("Linux {target} EGL window-surface creation failed"))
            .with_detail(format!(
                "platform_entry_available={entry_available}, platform_error={platform_error:?}, legacy_error={legacy_error:?}"
            ))
    })
}

#[derive(Debug)]
struct LinuxSystemEglBackend;
struct LinuxOffscreenEglDomain;
struct LinuxX11EglDomain;
struct LinuxWaylandEglDomain;

/// Which native display the provider binds EGL to.
///
/// X11 carries Migo's private render connection; Wayland retains the
/// caller-owned display under its separate public lifetime contract.
#[derive(Debug, Clone)]
enum LinuxDisplayTarget {
    /// Headless: no window server involved (see `offscreen_display`).
    Offscreen,
    /// Onscreen X11: Migo's private render connection.
    X11(Arc<X11RenderConnection>),
    /// Onscreen Wayland: the host's `wl_display*`.
    Wayland(NonNull<c_void>),
}

// SAFETY: the only raw pointer arm is the host-owned Wayland display, whose
// client library and public descriptor contract permit this EGL use. X11's
// cross-thread invariant is owned and documented by X11RenderConnection.
unsafe impl Send for LinuxDisplayTarget {}
unsafe impl Sync for LinuxDisplayTarget {}

#[derive(Debug)]
pub struct LinuxEglProvider {
    target: LinuxDisplayTarget,
}

impl Default for LinuxEglProvider {
    fn default() -> Self {
        Self {
            target: LinuxDisplayTarget::Offscreen,
        }
    }
}

impl LinuxEglProvider {
    /// Headless provider: pbuffer presentation on a display that needs no
    /// window server.
    pub fn offscreen() -> Self {
        Self::default()
    }

    /// Onscreen provider bound to Migo's private X11 render connection.
    fn x11(connection: Arc<X11RenderConnection>) -> Self {
        Self {
            target: LinuxDisplayTarget::X11(connection),
        }
    }

    /// Onscreen provider bound to a host-owned `wl_display*`.
    pub fn wayland(display: NonNull<c_void>) -> Self {
        Self {
            target: LinuxDisplayTarget::Wayland(display),
        }
    }
}

impl EglProvider for LinuxEglProvider {
    fn backend_id(&self) -> GraphicsBackendId {
        GraphicsBackendId::of::<LinuxSystemEglBackend>()
    }

    fn concurrency(&self) -> EglConcurrency {
        match &self.target {
            LinuxDisplayTarget::X11(_) => EglConcurrency::RenderThreadOnly,
            LinuxDisplayTarget::Offscreen | LinuxDisplayTarget::Wayland(_) => {
                EglConcurrency::SharedContexts
            }
        }
    }

    fn platform_identity(&self) -> PlatformIdentity {
        let backend_id = self.backend_id();
        match &self.target {
            LinuxDisplayTarget::Offscreen => {
                PlatformIdentity::new::<LinuxOffscreenEglDomain>(backend_id, 0)
            }
            LinuxDisplayTarget::X11(connection) => PlatformIdentity::new::<LinuxX11EglDomain>(
                backend_id,
                connection.display().as_ptr() as usize,
            ),
            LinuxDisplayTarget::Wayland(display) => PlatformIdentity::new::<LinuxWaylandEglDomain>(
                backend_id,
                display.as_ptr() as usize,
            ),
        }
    }

    fn label(&self) -> &str {
        match &self.target {
            LinuxDisplayTarget::Offscreen => "linux-system-egl",
            LinuxDisplayTarget::X11(_) => "linux-system-egl-x11",
            LinuxDisplayTarget::Wayland(_) => "linux-system-egl-wayland",
        }
    }

    fn load(&self) -> EngineResult<EglInstance> {
        let library = unsafe { libloading::Library::new(LINUX_EGL_LIBRARY) }.map_err(|error| {
            EngineError::new(ErrorCode::RenderBackendError)
                .with_msg("load Linux system EGL failed")
                .with_detail(format!("{LINUX_EGL_LIBRARY}: {error}"))
        })?;
        unsafe { EglInstance::load_required_from(library) }.map_err(|error| {
            EngineError::new(ErrorCode::RenderBackendError)
                .with_msg("resolve required Linux EGL symbols failed")
                .with_detail(format!("provider={}: {error:?}", self.label()))
        })
    }

    fn display(&self, egl: &EglInstance) -> EngineResult<egl::Display> {
        match &self.target {
            LinuxDisplayTarget::Offscreen => offscreen_display(egl, self.label()),
            // Naming the platform explicitly beats letting the driver infer it
            // from the pointer. A non-null proc address can still be a loader
            // stub for an unsupported platform, so failure also falls through
            // to the EGL 1.4 native X11 binding.
            LinuxDisplayTarget::X11(connection) => {
                native_platform_display(egl, EGL_PLATFORM_X11_EXT, connection.display(), "X11")
            }
            // EGL 1.4 Wayland bindings define EGLNativeDisplayType as
            // wl_display*, which is the compatibility path when the preferred
            // EGL 1.5/EXT platform call is absent or returns NO_DISPLAY.
            LinuxDisplayTarget::Wayland(display) => {
                native_platform_display(egl, EGL_PLATFORM_WAYLAND_EXT, *display, "Wayland")
            }
        }
    }

    fn presenter_surface_type(&self) -> egl::Int {
        match &self.target {
            // The offscreen presenter renders into a pbuffer; see
            // `LinuxPreparedSurface::create_window_surface`.
            LinuxDisplayTarget::Offscreen => egl::PBUFFER_BIT,
            LinuxDisplayTarget::X11(_) | LinuxDisplayTarget::Wayland(_) => egl::WINDOW_BIT,
        }
    }
}

/// The display a headless target renders on.
///
/// `eglGetDisplay(EGL_DEFAULT_DISPLAY)` is not headless on Mesa: it resolves to
/// the build's default window-system platform -- X11 on every distribution
/// build -- and `eglInitialize` then fails with EGL_NOT_INITIALIZED wherever
/// `DISPLAY` is unset, which is every CI runner and server. It only ever worked
/// on machines that happened to run a window server.
///
/// Mesa's surfaceless platform is the display that exists without one, so it is
/// taken whenever the client extensions advertise it. Its configs are all
/// pbuffer-only, which `presenter_surface_type` accounts for. A driver without
/// it keeps the default display, which is the only headless option EGL 1.4
/// gives it. The same order as wgpu-hal's GLES backend (`gles/egl.rs`), which
/// also asks surfaceless displays for pbuffer configs only.
fn offscreen_display(egl: &EglInstance, label: &str) -> EngineResult<egl::Display> {
    let surfaceless_advertised = egl
        .query_string(None, egl::EXTENSIONS)
        .is_ok_and(|extensions| {
            extensions
                .to_bytes()
                .split(|byte| *byte == b' ')
                .any(|name| name == b"EGL_MESA_platform_surfaceless")
        });
    let platform_entry = surfaceless_advertised
        .then(|| unsafe {
            platform_ext::first_entry_point::<platform_ext::GetPlatformDisplay>(
                egl,
                &GET_PLATFORM_DISPLAY_NAMES,
            )
        })
        .flatten();

    if let Some(get_platform_display) = platform_entry {
        // SAFETY: signature per EGL 1.5 / EXT_platform_base. The surfaceless
        // platform takes EGL_DEFAULT_DISPLAY as its native display, and a null
        // attribute list is valid for both entry-point variants.
        let raw = unsafe {
            get_platform_display(
                EGL_PLATFORM_SURFACELESS_MESA,
                egl::DEFAULT_DISPLAY,
                std::ptr::null(),
            )
        };
        return NonNull::new(raw)
            // SAFETY: non-null and produced by EGL itself.
            .map(|raw| unsafe { egl::Display::from_ptr(raw.as_ptr()) })
            .ok_or_else(|| {
                EngineError::new(ErrorCode::RenderInitializeError)
                    .with_msg("Linux surfaceless EGL display unavailable")
                    .with_detail(format!("provider={label}, error={:?}", egl.get_error()))
            });
    }

    unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }.ok_or_else(|| {
        EngineError::new(ErrorCode::RenderInitializeError)
            .with_msg("Linux eglGetDisplay failed")
            .with_detail(format!(
                "provider={label}, surfaceless_advertised={surfaceless_advertised}"
            ))
    })
}

/// Offscreen render target for headless Linux (pbuffer-backed). Carries only
/// the physical framebuffer size the presenter allocates the pbuffer to.
#[derive(Debug)]
pub struct LinuxOffscreenSurface {
    width: u32,
    height: u32,
}

impl LinuxOffscreenSurface {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

impl Surface for LinuxOffscreenSurface {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

/// Onscreen X11 render target. Carries the host's window and the physical size
/// the host mapped it at.
///
/// The host creates, resizes and destroys the `Window`; Migo renders through
/// its private connection to the same X11 server. The caller's `Display*` is
/// never retained here.
#[derive(Debug)]
pub struct LinuxX11Surface {
    connection: Arc<X11RenderConnection>,
    window: c_ulong,
    width: u32,
    height: u32,
}

impl LinuxX11Surface {
    fn new(connection: Arc<X11RenderConnection>, window: c_ulong, width: u32, height: u32) -> Self {
        Self {
            connection,
            window,
            width,
            height,
        }
    }
}

impl Surface for LinuxX11Surface {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

/// Minimal injectable Wayland EGL boundary. Safe wrappers contain all FFI and
/// let tests prove native-window ordering without a compositor.
trait WaylandEglApi: std::fmt::Debug + Send + Sync {
    fn create(&self, surface: NonNull<c_void>, width: i32, height: i32) -> Option<NonNull<c_void>>;
    fn resize(&self, window: NonNull<c_void>, width: i32, height: i32);
    fn destroy(&self, window: NonNull<c_void>);
}

/// The three `wl_egl_window` entry points, resolved once.
///
/// Declared here rather than taken from a header: the SDK carries no Wayland
/// build dependency, and these three signatures are the whole of what it needs.
mod wayland_egl {
    use super::*;
    use std::sync::{Arc, OnceLock};

    pub(super) type Create =
        unsafe extern "C" fn(*mut c_void, std::os::raw::c_int, std::os::raw::c_int) -> *mut c_void;
    pub(super) type Resize = unsafe extern "C" fn(
        *mut c_void,
        std::os::raw::c_int,
        std::os::raw::c_int,
        std::os::raw::c_int,
        std::os::raw::c_int,
    );
    pub(super) type Destroy = unsafe extern "C" fn(*mut c_void);

    pub(super) struct Glue {
        pub(super) create: Create,
        pub(super) resize: Resize,
        pub(super) destroy: Destroy,
        // Kept alive so the resolved pointers stay valid; never used directly.
        _library: libloading::Library,
    }

    // SAFETY: the three entries are plain function pointers into a library that
    // stays loaded for the process, and `libloading::Library` is itself Send +
    // Sync. Nothing here is mutated after construction.
    unsafe impl Send for Glue {}
    unsafe impl Sync for Glue {}

    impl std::fmt::Debug for Glue {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("WaylandEglGlue")
        }
    }

    impl super::WaylandEglApi for Glue {
        fn create(
            &self,
            surface: NonNull<c_void>,
            width: i32,
            height: i32,
        ) -> Option<NonNull<c_void>> {
            // SAFETY: the host keeps wl_surface alive and dimensions were
            // range-checked before this call.
            NonNull::new(unsafe { (self.create)(surface.as_ptr(), width, height) })
        }

        fn resize(&self, window: NonNull<c_void>, width: i32, height: i32) {
            // SAFETY: this Glue created the uniquely-owned window.
            unsafe { (self.resize)(window.as_ptr(), width, height, 0, 0) };
        }

        fn destroy(&self, window: NonNull<c_void>) {
            // SAFETY: the owner calls this exactly once after EGL detaches.
            unsafe { (self.destroy)(window.as_ptr()) };
        }
    }

    static GLUE: OnceLock<Option<Arc<Glue>>> = OnceLock::new();

    /// Resolve the glue, or `None` when this system has no Wayland EGL.
    ///
    /// Loaded lazily and once: a host that only ever attaches X11 or renders
    /// offscreen must not pay for, or fail on, a library it never needs.
    pub(super) fn glue() -> Option<Arc<dyn super::WaylandEglApi>> {
        GLUE.get_or_init(|| {
            let library = unsafe { libloading::Library::new(LINUX_WAYLAND_EGL_LIBRARY) }.ok()?;
            // SAFETY: the signatures above are the ones wayland-egl documents.
            unsafe {
                let create = *library.get::<Create>(b"wl_egl_window_create\0").ok()?;
                let resize = *library.get::<Resize>(b"wl_egl_window_resize\0").ok()?;
                let destroy = *library.get::<Destroy>(b"wl_egl_window_destroy\0").ok()?;
                Some(Arc::new(Glue {
                    create,
                    resize,
                    destroy,
                    _library: library,
                }))
            }
        })
        .as_ref()
        .map(|glue| Arc::clone(glue) as Arc<dyn super::WaylandEglApi>)
    }
}

/// Onscreen Wayland target. The `wl_surface` belongs to the host, which also
/// owns its role (xdg_toplevel or otherwise) and its event dispatch; this crate
/// only ever hands the pointer to `wl_egl_window_create`.
#[derive(Debug)]
pub struct LinuxWaylandSurface {
    display: NonNull<c_void>,
    surface: NonNull<c_void>,
    width: u32,
    height: u32,
}

// SAFETY: as for the X11 display handle -- an opaque token this crate passes on
// and never dereferences, so moving it between threads adds no aliasing. The
// host guarantees it outlives the attachment.
unsafe impl Send for LinuxWaylandSurface {}
unsafe impl Sync for LinuxWaylandSurface {}

impl LinuxWaylandSurface {
    /// `surface` is a host-owned `wl_surface*` already given a role and sized
    /// to `width` x `height` physical pixels.
    pub fn new(
        display: NonNull<c_void>,
        surface: NonNull<c_void>,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            display,
            surface,
            width,
            height,
        }
    }
}

impl Surface for LinuxWaylandSurface {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

struct WaylandEglWindow {
    handle: NonNull<c_void>,
    api: Arc<dyn WaylandEglApi>,
}

unsafe impl Send for WaylandEglWindow {}
unsafe impl Sync for WaylandEglWindow {}

impl std::fmt::Debug for WaylandEglWindow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WaylandEglWindow")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

impl Drop for WaylandEglWindow {
    fn drop(&mut self) {
        self.api.destroy(self.handle);
    }
}

#[derive(Debug)]
struct WaylandPreparedState {
    width: i32,
    height: i32,
    window: Option<WaylandEglWindow>,
}

/// Non-owning Wayland identity with one lazily materialized native window.
///
/// Identity includes both `wl_display` and `wl_surface`. The mutable state is
/// cold-path only and serializes create/resize against each other; frame
/// presentation never touches this mutex.
pub struct LinuxWaylandPreparedSurface {
    display: NonNull<c_void>,
    surface: NonNull<c_void>,
    state: parking_lot::Mutex<WaylandPreparedState>,
    api: Arc<dyn WaylandEglApi>,
}

// SAFETY: pointers are opaque tokens handed to EGL/wayland-egl and mutable
// state is serialized by the cold-path mutex.
unsafe impl Send for LinuxWaylandPreparedSurface {}
unsafe impl Sync for LinuxWaylandPreparedSurface {}

impl std::fmt::Debug for LinuxWaylandPreparedSurface {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LinuxWaylandPreparedSurface")
            .field("display", &self.display)
            .field("surface", &self.surface)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl LinuxWaylandPreparedSurface {
    fn new(
        display: NonNull<c_void>,
        surface: NonNull<c_void>,
        width: u32,
        height: u32,
        api: Arc<dyn WaylandEglApi>,
    ) -> EngineResult<Self> {
        let width = i32::try_from(width).map_err(|_| {
            EngineError::new(ErrorCode::InvalidOperation)
                .with_msg("Wayland Surface width exceeds wl_egl_window range")
        })?;
        let height = i32::try_from(height).map_err(|_| {
            EngineError::new(ErrorCode::InvalidOperation)
                .with_msg("Wayland Surface height exceeds wl_egl_window range")
        })?;
        Ok(Self {
            display,
            surface,
            state: parking_lot::Mutex::new(WaylandPreparedState {
                width,
                height,
                window: None,
            }),
            api,
        })
    }

    fn materialize_locked(
        &self,
        state: &mut WaylandPreparedState,
    ) -> EngineResult<NonNull<c_void>> {
        if let Some(window) = state.window.as_ref() {
            return Ok(window.handle);
        }
        let handle = self
            .api
            .create(self.surface, state.width, state.height)
            .ok_or_else(|| {
                EngineError::new(ErrorCode::RenderInitializeError)
                    .with_msg("wl_egl_window_create failed")
            })?;
        state.window = Some(WaylandEglWindow {
            handle,
            api: Arc::clone(&self.api),
        });
        Ok(handle)
    }

    fn materialize_native_window(&self) -> EngineResult<NonNull<c_void>> {
        let mut state = self.state.lock();
        self.materialize_locked(&mut state)
    }
}

impl PreparedEglSurface for LinuxWaylandPreparedSurface {
    fn backend_id(&self) -> GraphicsBackendId {
        GraphicsBackendId::of::<LinuxSystemEglBackend>()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn same_native_surface(&self, other: &dyn PreparedEglSurface) -> bool {
        other
            .as_any()
            .downcast_ref::<LinuxWaylandPreparedSurface>()
            .is_some_and(|other| self.display == other.display && self.surface == other.surface)
    }

    fn reconfigure_from(&self, candidate: &dyn PreparedEglSurface) -> EngineResult<()> {
        let candidate = candidate
            .as_any()
            .downcast_ref::<LinuxWaylandPreparedSurface>()
            .filter(|candidate| {
                self.display == candidate.display && self.surface == candidate.surface
            })
            .ok_or_else(|| {
                EngineError::new(ErrorCode::InvalidOperation)
                    .with_msg("Wayland resize candidate has a different display or surface")
            })?;
        if std::ptr::eq(self, candidate) {
            return Ok(());
        }
        let (width, height) = {
            let candidate = candidate.state.lock();
            (candidate.width, candidate.height)
        };
        let mut installed = self.state.lock();
        if let Some(window) = installed.window.as_ref() {
            self.api.resize(window.handle, width, height);
        }
        installed.width = width;
        installed.height = height;
        Ok(())
    }

    fn create_window_surface(
        &self,
        egl: &EglInstance,
        display: egl::Display,
        config: egl::Config,
    ) -> EngineResult<egl::Surface> {
        // The platform entry point takes a *pointer to* the native window, and
        // on Wayland the native window is the `wl_egl_window` -- not the
        // `wl_surface`. Passing the surface here compiles, links, and produces
        // a surface that never presents.
        // Calling conventions differ between platforms, and getting this wrong
        // fails at surface creation rather than anywhere useful. X11's native
        // window is an XID, so the EXT entry point takes a pointer *to* it.
        // Wayland's native window is already a pointer -- the wl_egl_window --
        // so it is passed directly. Wrapping it in another pointer, as the X11
        // path correctly does for its XID, makes EGL read the stack slot
        // holding the pointer as if it were the window.
        let mut state = self.state.lock();
        let egl_window = self.materialize_locked(&mut state)?;
        // EGL 1.4 and platform entry points both take wl_egl_window* by value.
        let created = create_native_window_surface(
            egl,
            display,
            config,
            egl_window.as_ptr(),
            egl_window.as_ptr(),
            "Wayland",
        );
        if created.is_err() {
            // EGL retained nothing on failure, so destroy the failed native
            // candidate now. A later retry will materialize a fresh wrapper.
            drop(state.window.take());
        }
        created
    }
}

#[derive(Clone, Debug)]
enum LinuxSurfaceFactoryTarget {
    Offscreen,
    X11(Arc<X11RenderConnection>),
    Wayland(NonNull<c_void>),
}

unsafe impl Send for LinuxSurfaceFactoryTarget {}
unsafe impl Sync for LinuxSurfaceFactoryTarget {}

#[derive(Debug)]
pub struct LinuxEglSurfaceFactory {
    target: LinuxSurfaceFactoryTarget,
}

impl LinuxEglSurfaceFactory {
    fn offscreen() -> Self {
        Self {
            target: LinuxSurfaceFactoryTarget::Offscreen,
        }
    }

    fn x11(connection: Arc<X11RenderConnection>) -> Self {
        Self {
            target: LinuxSurfaceFactoryTarget::X11(connection),
        }
    }

    fn wayland(display: NonNull<c_void>) -> Self {
        Self {
            target: LinuxSurfaceFactoryTarget::Wayland(display),
        }
    }
}

impl EglSurfaceFactory for LinuxEglSurfaceFactory {
    fn backend_id(&self) -> GraphicsBackendId {
        GraphicsBackendId::of::<LinuxSystemEglBackend>()
    }

    fn platform_identity(&self) -> PlatformIdentity {
        let backend_id = self.backend_id();
        match &self.target {
            LinuxSurfaceFactoryTarget::Offscreen => {
                PlatformIdentity::new::<LinuxOffscreenEglDomain>(backend_id, 0)
            }
            LinuxSurfaceFactoryTarget::X11(connection) => {
                PlatformIdentity::new::<LinuxX11EglDomain>(
                    backend_id,
                    connection.display().as_ptr() as usize,
                )
            }
            LinuxSurfaceFactoryTarget::Wayland(display) => PlatformIdentity::new::<
                LinuxWaylandEglDomain,
            >(
                backend_id, display.as_ptr() as usize
            ),
        }
    }

    fn prepare(&self, surface: &dyn Surface) -> EngineResult<PreparedEglSurfaceRef> {
        let any = surface.as_any();
        if let (LinuxSurfaceFactoryTarget::Offscreen, Some(offscreen)) =
            (&self.target, any.downcast_ref::<LinuxOffscreenSurface>())
        {
            return Ok(Arc::new(LinuxPreparedSurface {
                width: offscreen.width,
                height: offscreen.height,
            }));
        }
        if let (LinuxSurfaceFactoryTarget::X11(connection), Some(x11)) =
            (&self.target, any.downcast_ref::<LinuxX11Surface>())
        {
            if !Arc::ptr_eq(connection, &x11.connection) {
                return Err(EngineError::new(ErrorCode::InvalidOperation)
                    .with_msg("X11 Surface does not belong to this render connection"));
            }
            return Ok(Arc::new(LinuxX11PreparedSurface {
                connection: Arc::clone(connection),
                window: x11.window,
            }));
        }
        if let (LinuxSurfaceFactoryTarget::Wayland(display), Some(wayland)) =
            (&self.target, any.downcast_ref::<LinuxWaylandSurface>())
        {
            if *display != wayland.display {
                return Err(EngineError::new(ErrorCode::InvalidOperation)
                    .with_msg("Wayland Surface display does not match EGL platform display"));
            }
            let api = wayland_egl::glue().ok_or_else(|| {
                EngineError::new(ErrorCode::Unsupported)
                    .with_msg("Wayland surface attached but the Wayland EGL glue is absent")
                    .with_detail(format!("{LINUX_WAYLAND_EGL_LIBRARY} could not be loaded"))
            })?;
            return Ok(Arc::new(LinuxWaylandPreparedSurface::new(
                *display,
                wayland.surface,
                wayland.width,
                wayland.height,
                api,
            )?));
        }
        Err(EngineError::new(ErrorCode::Unsupported)
            .with_msg("Surface kind does not match the Linux EGL platform")
            .with_detail(format!("factory={:?}, surface={surface:?}", self.target)))
    }
}

/// Non-owning onscreen target. Identity is Display plus Window: XIDs are scoped
/// to a connection and cannot be compared safely without that Display.
#[derive(Debug)]
pub struct LinuxX11PreparedSurface {
    connection: Arc<X11RenderConnection>,
    window: c_ulong,
}

impl PreparedEglSurface for LinuxX11PreparedSurface {
    fn backend_id(&self) -> GraphicsBackendId {
        GraphicsBackendId::of::<LinuxSystemEglBackend>()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn same_native_surface(&self, other: &dyn PreparedEglSurface) -> bool {
        other
            .as_any()
            .downcast_ref::<LinuxX11PreparedSurface>()
            .is_some_and(|other| {
                Arc::ptr_eq(&self.connection, &other.connection) && self.window == other.window
            })
    }

    fn create_window_surface(
        &self,
        egl: &EglInstance,
        display: egl::Display,
        config: egl::Config,
    ) -> EngineResult<egl::Surface> {
        // Mind the calling conventions: the platform entry point takes a
        // *pointer to* the native window, while legacy `eglCreateWindowSurface`
        // takes the XID by value. Passing one where the other is expected reads
        // the XID as an address.
        let window = self.window;
        create_native_window_surface(
            egl,
            display,
            config,
            &window as *const c_ulong as *mut c_void,
            window as *mut c_void,
            &format!("X11 window 0x{window:x}"),
        )
    }
}

/// Non-owning, repeatable offscreen target. A pbuffer has no native handle, so
/// identity is the (width, height) it was prepared for.
#[derive(Debug)]
pub struct LinuxPreparedSurface {
    width: u32,
    height: u32,
}

impl PreparedEglSurface for LinuxPreparedSurface {
    fn backend_id(&self) -> GraphicsBackendId {
        GraphicsBackendId::of::<LinuxSystemEglBackend>()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn same_native_surface(&self, other: &dyn PreparedEglSurface) -> bool {
        other
            .as_any()
            .downcast_ref::<LinuxPreparedSurface>()
            .is_some_and(|other| self.width == other.width && self.height == other.height)
    }

    fn create_window_surface(
        &self,
        egl: &EglInstance,
        display: egl::Display,
        config: egl::Config,
    ) -> EngineResult<egl::Surface> {
        // Offscreen: the "window surface" the render binding requests is served
        // by a pbuffer sized to the target, so no window server is needed.
        let attributes = [
            egl::WIDTH,
            self.width as egl::Int,
            egl::HEIGHT,
            self.height as egl::Int,
            egl::NONE,
        ];
        egl.create_pbuffer_surface(display, config, &attributes)
            .map_err(|error| {
                EngineError::new(ErrorCode::RenderBackendError)
                    .with_msg("Linux eglCreatePbufferSurface failed")
                    .with_detail(format!("{error:?}"))
            })
    }
}

/// Offscreen Linux graphics platform: system EGL + pbuffer surface factory.
pub fn linux_graphics_platform() -> EngineResult<GraphicsPlatform> {
    GraphicsPlatform::try_new(
        Arc::new(LinuxEglProvider::offscreen()),
        Arc::new(LinuxEglSurfaceFactory::offscreen()),
    )
}

/// Session-scoped owner for one private X11 render connection.
///
/// The host's event connection is borrowed only while `open` or
/// `supports_host_display` executes. Surfaces created here retain the private
/// connection structurally, so no EGL target can outlive it.
#[derive(Clone, Debug)]
pub struct LinuxX11Context {
    connection: Arc<X11RenderConnection>,
    graphics_platform: GraphicsPlatform,
}

impl LinuxX11Context {
    fn from_connection(connection: Arc<X11RenderConnection>) -> EngineResult<Self> {
        let graphics_platform = GraphicsPlatform::try_new(
            Arc::new(LinuxEglProvider::x11(Arc::clone(&connection))),
            Arc::new(LinuxEglSurfaceFactory::x11(Arc::clone(&connection))),
        )?;
        Ok(Self {
            connection,
            graphics_platform,
        })
    }

    /// Resolve the host connection's server and open Migo's render connection.
    ///
    /// # Safety
    /// `host_display` must be a live Xlib `Display*` for this call.
    pub unsafe fn open(host_display: NonNull<c_void>) -> EngineResult<Self> {
        let connection = unsafe { X11RenderConnection::open(host_display) }?;
        Self::from_connection(connection)
    }

    /// Check that a later host connection reaches the same live X11 server.
    ///
    /// # Safety
    /// `host_display` must be a live Xlib `Display*` for this call.
    pub unsafe fn supports_host_display(&self, host_display: NonNull<c_void>) -> EngineResult<()> {
        unsafe { self.connection.supports_host_display(host_display) }
    }

    pub fn graphics_platform(&self) -> GraphicsPlatform {
        self.graphics_platform.clone()
    }

    pub fn surface(&self, window: c_ulong, width: u32, height: u32) -> SurfaceRef {
        Arc::new(LinuxX11Surface::new(
            Arc::clone(&self.connection),
            window,
            width,
            height,
        ))
    }

    #[cfg(test)]
    pub(crate) fn render_display_for_test(&self) -> NonNull<c_void> {
        self.connection.display()
    }

    #[cfg(test)]
    pub(crate) fn connection_fd_for_test(&self) -> std::os::fd::RawFd {
        self.connection.fd_for_test()
    }

    /// Open a render context against a declared topology instead of a server.
    ///
    /// Reachable from other crates under `test-support` because the C ABI layer
    /// owns the decision this exists to test -- whether a reattachment reuses
    /// this connection or opens another -- and that decision is two crates from
    /// the connection itself.
    #[cfg(any(test, feature = "test-support"))]
    pub fn open_on_test_servers(
        servers: &X11TestServers,
        host: NonNull<c_void>,
        render: NonNull<c_void>,
    ) -> EngineResult<Self> {
        Self::from_connection(servers.open_connection(host, render)?)
    }
}

/// Onscreen Wayland platform bound to a host-owned `wl_display*`.
///
/// The host owns the connection, the surface's role and its event dispatch;
/// this crate only hands the display to EGL. The pointer must stay valid for
/// the whole attachment.
pub fn linux_wayland_graphics_platform(display: NonNull<c_void>) -> EngineResult<GraphicsPlatform> {
    GraphicsPlatform::try_new(
        Arc::new(LinuxEglProvider::wayland(display)),
        Arc::new(LinuxEglSurfaceFactory::wayland(display)),
    )
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;

    use super::*;

    /// A context whose private render connection is exactly `display`, opened
    /// from a separate host display on the same server -- the shape production
    /// `open` resolves.
    fn test_x11_context(display: NonNull<c_void>) -> LinuxX11Context {
        const TEST_SERVER: u8 = 7;
        let host = NonNull::new(0x1usize as *mut c_void).expect("host display");
        let mut servers = X11TestServers::new();
        servers.place(host, TEST_SERVER).place(display, TEST_SERVER);
        LinuxX11Context::open_on_test_servers(&servers, host, display).expect("test X11 context")
    }

    #[test]
    fn egl15_core_entry_points_are_preferred_before_ext_aliases() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&calls);
        let resolved = platform_ext::first_resolved(&GET_PLATFORM_DISPLAY_NAMES, move |name| {
            seen.lock().push(name.to_string());
            (name == "eglGetPlatformDisplay").then_some(7_u8)
        });
        assert_eq!(resolved, Some(7));
        assert_eq!(&*calls.lock(), &["eglGetPlatformDisplay"]);

        let resolved =
            platform_ext::first_resolved(&CREATE_PLATFORM_WINDOW_SURFACE_NAMES, |name| {
                (name.ends_with("EXT")).then_some(9_u8)
            });
        assert_eq!(resolved, Some(9), "EXT remains the EGL 1.4 fallback");
    }

    #[derive(Debug)]
    struct FakeWaylandEgl {
        events: Arc<Mutex<Vec<&'static str>>>,
        window: NonNull<c_void>,
    }

    unsafe impl Send for FakeWaylandEgl {}
    unsafe impl Sync for FakeWaylandEgl {}

    impl WaylandEglApi for FakeWaylandEgl {
        fn create(
            &self,
            _surface: NonNull<c_void>,
            _width: i32,
            _height: i32,
        ) -> Option<NonNull<c_void>> {
            self.events.lock().push("native-create");
            Some(self.window)
        }

        fn resize(&self, _window: NonNull<c_void>, _width: i32, _height: i32) {
            self.events.lock().push("native-resize");
        }

        fn destroy(&self, _window: NonNull<c_void>) {
            self.events.lock().push("native-destroy");
        }
    }

    #[test]
    fn wayland_window_is_lazy_resized_in_place_and_destroyed_after_egl() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let api: Arc<dyn WaylandEglApi> = Arc::new(FakeWaylandEgl {
            events: Arc::clone(&events),
            window: NonNull::new(0x6a6a_0001usize as *mut c_void).unwrap(),
        });
        let display = NonNull::new(0x5a5a_1000usize as *mut c_void).unwrap();
        let surface = NonNull::new(0x5a5a_0001usize as *mut c_void).unwrap();
        let installed =
            LinuxWaylandPreparedSurface::new(display, surface, 320, 240, Arc::clone(&api)).unwrap();
        assert!(events.lock().is_empty(), "prepare must stay lazy");

        installed.materialize_native_window().unwrap();
        let candidate = LinuxWaylandPreparedSurface::new(display, surface, 1280, 720, api).unwrap();
        installed.reconfigure_from(&candidate).unwrap();
        drop(candidate);
        assert_eq!(&*events.lock(), &["native-create", "native-resize"]);

        // CanvasManager's successful EGL teardown happens before it drops the
        // installed PreparedEglSurface. Record that external boundary here.
        events.lock().push("egl-destroy");
        drop(installed);
        assert_eq!(
            &*events.lock(),
            &[
                "native-create",
                "native-resize",
                "egl-destroy",
                "native-destroy"
            ]
        );
    }

    /// Two Wayland surfaces are the same native surface exactly when they wrap
    /// the same `wl_surface`, so a resize keeps the attachment and a different
    /// surface never inherits it.
    ///
    /// Checked on the plain Surface because identity is a property of what the
    /// host handed over, not of the lazily-created EGL wrapper.
    #[test]
    fn wayland_surface_identity_is_the_surface_not_the_size() {
        let handle = NonNull::new(0x5a5a_0001usize as *mut c_void).expect("token");
        let other = NonNull::new(0x5a5a_0002usize as *mut c_void).expect("token");
        let display = NonNull::new(0x5a5a_1000usize as *mut c_void).expect("display");

        let small = LinuxWaylandSurface::new(display, handle, 320, 240);
        let large = LinuxWaylandSurface::new(display, handle, 1280, 720);
        let different = LinuxWaylandSurface::new(display, other, 320, 240);

        assert_eq!(small.surface, large.surface, "a resize is the same surface");
        assert_ne!(small.size(), large.size());
        assert_ne!(
            small.surface, different.surface,
            "a different wl_surface must never compare equal"
        );
    }

    /// A Wayland surface must not be mistaken for an X11 one: the factory
    /// downcasts, and an arm that matched the wrong type would hand EGL a
    /// pointer of the wrong kind.
    #[test]
    fn wayland_and_x11_surfaces_are_never_confused() {
        let display = NonNull::new(0x5a5a_1000usize as *mut c_void).expect("display");
        let wayland = LinuxWaylandSurface::new(
            display,
            NonNull::new(0x5a5a_0001usize as *mut c_void).expect("token"),
            640,
            480,
        );
        let x11 = test_x11_context(display).surface(0x2a0_0001, 640, 480);

        assert!(wayland.as_any().downcast_ref::<LinuxX11Surface>().is_none());
        assert!(x11.as_any().downcast_ref::<LinuxWaylandSurface>().is_none());
    }

    /// The offscreen presenter's surface is a pbuffer, so its config must not
    /// require a window: a surfaceless display publishes no window config, and
    /// asking for one there fails every candidate.
    #[test]
    fn only_onscreen_providers_require_a_window_config() {
        let display = NonNull::new(0xdead_beefusize as *mut c_void).expect("token");
        assert_eq!(
            LinuxEglProvider::offscreen().presenter_surface_type(),
            egl::PBUFFER_BIT
        );
        assert_eq!(
            LinuxEglProvider::wayland(display).presenter_surface_type(),
            egl::WINDOW_BIT
        );
        assert_eq!(
            LinuxEglProvider::x11(test_x11_context(display).connection).presenter_surface_type(),
            egl::WINDOW_BIT
        );
    }

    #[test]
    fn wayland_platform_pairs_provider_and_factory() {
        let display = NonNull::new(0xdead_beefusize as *mut c_void).expect("token");
        let platform = linux_wayland_graphics_platform(display).expect("wayland graphics platform");
        assert_eq!(
            platform.egl_provider().backend_id(),
            platform.surface_factory().backend_id(),
        );
        assert_eq!(platform.egl_provider().label(), "linux-system-egl-wayland");
    }

    #[test]
    fn platform_identity_distinguishes_linux_domain_and_display() {
        let display = NonNull::new(0xdead_beefusize as *mut c_void).expect("display");
        let other_display = NonNull::new(0xcafe_babeusize as *mut c_void).expect("other display");
        let offscreen = linux_graphics_platform()
            .expect("offscreen platform")
            .platform_identity();
        let context = test_x11_context(display);
        let x11 = context.graphics_platform().platform_identity();
        let same_x11 = context.graphics_platform().platform_identity();
        let other_x11 = test_x11_context(other_display)
            .graphics_platform()
            .platform_identity();
        let wayland = linux_wayland_graphics_platform(display)
            .expect("Wayland platform")
            .platform_identity();

        assert_eq!(x11, same_x11);
        assert_ne!(x11, other_x11);
        assert_ne!(x11, wayland);
        assert_ne!(offscreen, x11);
        assert_ne!(offscreen, wayland);
    }

    #[test]
    fn platform_identity_rejects_mixed_linux_provider_and_factory() {
        let display = NonNull::new(0xdead_beefusize as *mut c_void).expect("display");
        let context = test_x11_context(display);
        let result = GraphicsPlatform::try_new(
            Arc::new(LinuxEglProvider::x11(Arc::clone(&context.connection))),
            Arc::new(LinuxEglSurfaceFactory::wayland(display)),
        );

        assert!(result.is_err());
    }

    #[test]
    fn provider_and_factory_share_backend_id() {
        // GraphicsPlatform::try_new fails closed on mismatched backend ids;
        // constructing it proves the Linux provider/factory are paired.
        let platform = linux_graphics_platform().expect("linux graphics platform");
        assert_eq!(
            platform.egl_provider().backend_id(),
            platform.surface_factory().backend_id(),
        );
        assert_eq!(platform.egl_provider().label(), "linux-system-egl");
    }

    #[test]
    fn offscreen_surface_reports_its_size() {
        let surface = LinuxOffscreenSurface::new(320, 240);
        assert_eq!(surface.size(), (320, 240));
        let prepared = LinuxEglSurfaceFactory::offscreen()
            .prepare(&surface)
            .expect("prepare offscreen");
        assert!(prepared.same_native_surface(prepared.as_ref()));
    }

    // ---- X11 onscreen target ----

    #[test]
    fn x11_context_binds_identity_surface_and_factory_to_one_owned_connection() {
        let render_display = NonNull::new(0x5a5a_1000usize as *mut c_void).expect("render display");
        let other_render_display =
            NonNull::new(0x5a5a_2000usize as *mut c_void).expect("other render display");
        let context = test_x11_context(render_display);
        let same_context = context.clone();
        let other_context = test_x11_context(other_render_display);

        let platform = context.graphics_platform();
        let same_platform = same_context.graphics_platform();
        let surface = context.surface(0x2a0_0001, 800, 600);
        let prepared = platform
            .prepare_surface(surface.as_ref())
            .expect("owned surface must prepare");

        assert_eq!(
            platform.egl_provider().concurrency(),
            EglConcurrency::RenderThreadOnly
        );
        assert_eq!(
            platform.platform_identity(),
            same_platform.platform_identity()
        );
        assert_eq!(surface.size(), (800, 600));
        assert!(prepared.same_native_surface(prepared.as_ref()));

        let foreign_surface = other_context.surface(0x2a0_0001, 800, 600);
        let error = platform
            .prepare_surface(foreign_surface.as_ref())
            .expect_err("another owned connection must fail closed");
        assert_eq!(error.code, ErrorCode::InvalidOperation);
    }

    #[test]
    fn x11_surface_prepares_and_reports_its_size() {
        let display = NonNull::new(0x5a5a_1000usize as *mut c_void).expect("display");
        let context = test_x11_context(display);
        let surface = context.surface(0x2a0_0001, 800, 600);
        assert_eq!(surface.size(), (800, 600));
        let prepared = LinuxEglSurfaceFactory::x11(Arc::clone(&context.connection))
            .prepare(surface.as_ref())
            .expect("prepare x11");
        assert!(prepared.same_native_surface(prepared.as_ref()));
    }

    #[test]
    fn x11_window_identity_is_display_plus_xid_not_size() {
        // A window keeps its identity across a resize, and two windows that
        // happen to share a size are still different surfaces. Getting this
        // wrong would let the render binding reuse a dead EGLSurface.
        let display = NonNull::new(0x5a5a_1000usize as *mut c_void).expect("display");
        let other_display = NonNull::new(0x5a5a_2000usize as *mut c_void).expect("other display");
        let context = test_x11_context(display);
        let other_context = test_x11_context(other_display);
        let prepare = |context: &LinuxX11Context, window, w, h| {
            LinuxEglSurfaceFactory::x11(Arc::clone(&context.connection))
                .prepare(context.surface(window, w, h).as_ref())
                .expect("prepare x11")
        };
        let resized = prepare(&context, 0x2a0_0001, 1024, 768);
        assert!(prepare(&context, 0x2a0_0001, 800, 600).same_native_surface(resized.as_ref()));
        assert!(
            !prepare(&context, 0x2a0_0002, 800, 600)
                .same_native_surface(prepare(&context, 0x2a0_0001, 800, 600).as_ref())
        );
        assert!(
            !prepare(&other_context, 0x2a0_0001, 800, 600).same_native_surface(resized.as_ref())
        );
    }

    #[test]
    fn offscreen_and_x11_targets_are_never_the_same_surface() {
        let display = NonNull::new(0x5a5a_1000usize as *mut c_void).expect("display");
        let context = test_x11_context(display);
        let offscreen = LinuxEglSurfaceFactory::offscreen()
            .prepare(&LinuxOffscreenSurface::new(800, 600))
            .expect("prepare offscreen");
        let x11 = LinuxEglSurfaceFactory::x11(Arc::clone(&context.connection))
            .prepare(context.surface(0x2a0_0001, 800, 600).as_ref())
            .expect("prepare x11");
        assert!(!offscreen.same_native_surface(x11.as_ref()));
        assert!(!x11.same_native_surface(offscreen.as_ref()));
    }

    #[test]
    fn foreign_surface_types_still_fail_closed() {
        // The factory must reject anything it does not own rather than guess a
        // native handle out of it.
        #[derive(Debug)]
        struct ForeignSurface;
        impl Surface for ForeignSurface {
            fn as_any(&self) -> &dyn Any {
                self
            }
            fn size(&self) -> (u32, u32) {
                (1, 1)
            }
        }
        let error = LinuxEglSurfaceFactory::offscreen()
            .prepare(&ForeignSurface)
            .expect_err("foreign surface must be rejected");
        assert_eq!(error.code, ErrorCode::Unsupported);
    }

    #[test]
    fn x11_platform_pairs_provider_and_factory() {
        // Same fail-closed pairing check as the offscreen platform: a mismatched
        // backend id would make GraphicsPlatform::try_new refuse.
        let display = NonNull::new(0xdead_beef_usize as *mut c_void).expect("non-null");
        let platform = test_x11_context(display).graphics_platform();
        assert_eq!(
            platform.egl_provider().backend_id(),
            platform.surface_factory().backend_id(),
        );
        assert_eq!(platform.egl_provider().label(), "linux-system-egl-x11");
    }
}
