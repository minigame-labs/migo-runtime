//! Cold-path platform boundary for EGL loading and native surface creation.
//!
//! The provider/factory pair is validated once with process-local type identity.
//! No method in this module participates in draw or present hot paths.

use std::{
    any::{Any, TypeId},
    fmt::Debug,
    sync::Arc,
};

use khronos_egl as egl;
use shared::{
    error::{EngineError, EngineResult, ErrorCode},
    surface::{Surface, SurfaceLease, SurfaceResourceLease},
};

pub type EglInstance = egl::DynamicInstance<egl::EGL1_4>;

/// Whether one provider may back EGL contexts used by multiple Migo threads.
///
/// This is a cold-path construction policy, not a driver capability inferred
/// at runtime. Providers must choose explicitly because using a native display
/// connection from a second thread can be unsound even when EGL itself exposes
/// shared contexts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EglConcurrency {
    RenderThreadOnly,
    SharedContexts,
}

/// Process-local backend identity derived only from a private concrete marker.
/// It is deliberately not a serialized/public ABI identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GraphicsBackendId(TypeId);

impl GraphicsBackendId {
    pub fn of<T: 'static>() -> Self {
        Self(TypeId::of::<T>())
    }
}

/// Immutable process-local identity for one compatible graphics domain.
///
/// The domain marker distinguishes native API families that share a backend
/// implementation, while `native_instance` distinguishes concrete displays or
/// devices within that family. This value is intentionally opaque and never
/// serialized across processes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlatformIdentity {
    backend_id: GraphicsBackendId,
    domain_id: TypeId,
    native_instance: usize,
}

impl PlatformIdentity {
    pub fn new<Domain: 'static>(backend_id: GraphicsBackendId, native_instance: usize) -> Self {
        Self {
            backend_id,
            domain_id: TypeId::of::<Domain>(),
            native_instance,
        }
    }

    #[inline]
    pub const fn backend_id(self) -> GraphicsBackendId {
        self.backend_id
    }
}

pub trait EglProvider: Debug + Send + Sync {
    fn backend_id(&self) -> GraphicsBackendId;
    fn concurrency(&self) -> EglConcurrency;
    fn platform_identity(&self) -> PlatformIdentity;
    fn label(&self) -> &str;
    fn load(&self) -> EngineResult<EglInstance>;
    fn display(&self, egl: &EglInstance) -> EngineResult<egl::Display>;

    /// The `EGL_SURFACE_TYPE` bit the presenter's surface needs from the shared
    /// config, on top of the pbuffer bit the offscreen contexts need.
    ///
    /// A window by default. A provider whose presenter renders into a pbuffer
    /// returns `PBUFFER_BIT`: requiring a window bit there would reject every
    /// config of a display that has no window system at all, such as Mesa's
    /// surfaceless platform, whose configs are all pbuffer-only.
    fn presenter_surface_type(&self) -> egl::Int {
        egl::WINDOW_BIT
    }
}

pub trait EglSurfaceFactory: Debug + Send + Sync {
    fn backend_id(&self) -> GraphicsBackendId;
    fn platform_identity(&self) -> PlatformIdentity;

    /// Convert the platform Surface into a non-owning presenter target.
    /// A failed concrete downcast must return `Unsupported`.
    fn prepare(&self, surface: &dyn Surface) -> EngineResult<PreparedEglSurfaceRef>;
}

pub trait PreparedEglSurface: Debug + Send + Sync {
    fn backend_id(&self) -> GraphicsBackendId;
    fn as_any(&self) -> &dyn Any;

    /// Return false when concrete types differ or a downcast fails.
    fn same_native_surface(&self, other: &dyn PreparedEglSurface) -> bool;

    /// Reconfigure this already-installed native target from an equivalent
    /// candidate without replacing the object EGL references.
    ///
    /// Backends whose native target needs no explicit resize use this checked
    /// default. Wayland overrides it to resize the unique `wl_egl_window`.
    fn reconfigure_from(&self, candidate: &dyn PreparedEglSurface) -> EngineResult<()> {
        if self.same_native_surface(candidate) {
            Ok(())
        } else {
            Err(EngineError::new(ErrorCode::InvalidOperation)
                .with_msg("cannot reconfigure from a different native Surface"))
        }
    }

    /// Repeatable native surface creation. Returning `Err` guarantees this
    /// call retained no newly-created EGL object.
    fn create_window_surface(
        &self,
        egl: &EglInstance,
        display: egl::Display,
        config: egl::Config,
    ) -> EngineResult<egl::Surface>;

    /// Tell the display what frame rate content is presenting at, so it can pick
    /// a mode that serves it without decimation.
    ///
    /// This is the difference between absorbing a display mismatch and removing
    /// it. A 60fps request on a 90Hz panel can only be decimated as 1,2,1,2
    /// vsyncs -- no scheduler tolerance makes that even, because two thirds of 90
    /// is not a whole number of frames. Asking the display for 60 instead makes
    /// the vsyncs themselves arrive at 60, which is both even and cheaper: a
    /// faster mode nobody is using still costs composition passes and panel power.
    ///
    /// Advisory, and a no-op by default: most platforms expose no way to ask, and
    /// one that does may decline. The frame scheduler remains the authority on
    /// what to present and stays correct for whatever the display delivers.
    fn request_frame_rate(&self, _fps: u32) {}
}

pub type PreparedEglSurfaceRef = Arc<dyn PreparedEglSurface>;

/// Structural pairing between a prepared platform target and the attachment
/// resource it may reach. Field order makes the native target drop before its
/// final resource lease.
struct ResourceBoundPreparedSurface {
    inner: PreparedEglSurfaceRef,
    _resource: SurfaceResourceLease,
}

impl Debug for ResourceBoundPreparedSurface {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResourceBoundPreparedSurface")
            .field("inner", &self.inner)
            .field("resource", &self._resource)
            .finish()
    }
}

impl PreparedEglSurface for ResourceBoundPreparedSurface {
    fn backend_id(&self) -> GraphicsBackendId {
        self.inner.backend_id()
    }

    fn as_any(&self) -> &dyn Any {
        // Platform comparisons/downcasts intentionally see the concrete inner
        // target, not this ownership-only wrapper.
        self.inner.as_any()
    }

    fn same_native_surface(&self, other: &dyn PreparedEglSurface) -> bool {
        self.inner.same_native_surface(other)
    }

    fn reconfigure_from(&self, candidate: &dyn PreparedEglSurface) -> EngineResult<()> {
        self.inner.reconfigure_from(candidate)
    }

    fn create_window_surface(
        &self,
        egl: &EglInstance,
        display: egl::Display,
        config: egl::Config,
    ) -> EngineResult<egl::Surface> {
        self.inner.create_window_surface(egl, display, config)
    }
}

#[derive(Clone, Debug)]
pub struct GraphicsPlatform {
    egl_provider: Arc<dyn EglProvider>,
    surface_factory: Arc<dyn EglSurfaceFactory>,
    backend_id: GraphicsBackendId,
    platform_identity: PlatformIdentity,
}

impl GraphicsPlatform {
    pub fn try_new(
        egl_provider: Arc<dyn EglProvider>,
        surface_factory: Arc<dyn EglSurfaceFactory>,
    ) -> EngineResult<Self> {
        let provider_id = egl_provider.backend_id();
        let factory_id = surface_factory.backend_id();
        let provider_identity = egl_provider.platform_identity();
        let factory_identity = surface_factory.platform_identity();
        if provider_id != factory_id
            || provider_identity.backend_id() != provider_id
            || factory_identity.backend_id() != factory_id
            || provider_identity != factory_identity
        {
            return Err(EngineError::new(ErrorCode::InvalidOperation)
                .with_msg("incompatible EGL provider and surface factory identity")
                .with_detail(format!(
                    "provider={} ({provider_id:?}, {provider_identity:?}), factory=({factory_id:?}, {factory_identity:?})",
                    egl_provider.label(),
                )));
        }
        Ok(Self {
            egl_provider,
            surface_factory,
            backend_id: provider_id,
            platform_identity: provider_identity,
        })
    }

    #[inline]
    pub fn backend_id(&self) -> GraphicsBackendId {
        self.backend_id
    }

    #[inline]
    pub const fn platform_identity(&self) -> PlatformIdentity {
        self.platform_identity
    }

    #[inline]
    pub fn egl_provider(&self) -> &Arc<dyn EglProvider> {
        &self.egl_provider
    }

    #[inline]
    pub fn surface_factory(&self) -> &Arc<dyn EglSurfaceFactory> {
        &self.surface_factory
    }

    pub fn prepare_surface(&self, surface: &dyn Surface) -> EngineResult<PreparedEglSurfaceRef> {
        let prepared = self.surface_factory.prepare(surface)?;
        self.validate_prepared(prepared.as_ref())?;
        Ok(prepared)
    }

    /// Prepare a platform target and structurally bind it to the native
    /// resource lease before it can reach EGL.
    pub fn prepare_surface_for_lease(
        &self,
        lease: &SurfaceLease,
    ) -> EngineResult<PreparedEglSurfaceRef> {
        let prepared = self.prepare_surface(lease.surface().as_ref())?;
        Ok(Arc::new(ResourceBoundPreparedSurface {
            inner: prepared,
            _resource: lease.resource_lease(),
        }))
    }

    pub fn validate_prepared(&self, prepared: &dyn PreparedEglSurface) -> EngineResult<()> {
        if prepared.backend_id() != self.backend_id {
            return Err(EngineError::new(ErrorCode::Unsupported)
                .with_msg("prepared EGL surface belongs to another backend")
                .with_detail(format!(
                    "platform={:?}, prepared={:?}",
                    self.backend_id,
                    prepared.backend_id()
                )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        marker::PhantomData,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use super::*;

    #[derive(Debug)]
    struct BackendA;
    #[derive(Debug)]
    struct BackendB;
    #[derive(Debug)]
    struct DomainA;
    #[derive(Debug)]
    struct DomainB;

    #[derive(Debug)]
    struct FakeProvider<T> {
        marker: PhantomData<fn() -> T>,
    }

    impl<T> FakeProvider<T> {
        fn new() -> Self {
            Self {
                marker: PhantomData,
            }
        }
    }

    impl<T: 'static + Debug> EglProvider for FakeProvider<T> {
        fn backend_id(&self) -> GraphicsBackendId {
            GraphicsBackendId::of::<T>()
        }

        fn concurrency(&self) -> EglConcurrency {
            EglConcurrency::SharedContexts
        }

        fn platform_identity(&self) -> PlatformIdentity {
            PlatformIdentity::new::<T>(self.backend_id(), 0)
        }

        fn label(&self) -> &str {
            std::any::type_name::<T>()
        }

        fn load(&self) -> EngineResult<EglInstance> {
            Err(EngineError::new(ErrorCode::RenderInitializeError))
        }

        fn display(&self, _egl: &EglInstance) -> EngineResult<egl::Display> {
            Err(EngineError::new(ErrorCode::RenderInitializeError))
        }
    }

    #[derive(Debug)]
    struct FakeFactory<FactoryBackend, TargetBackend> {
        native_creates: Arc<AtomicUsize>,
        marker: PhantomData<fn() -> (FactoryBackend, TargetBackend)>,
    }

    impl<F, T> FakeFactory<F, T> {
        fn new(native_creates: Arc<AtomicUsize>) -> Self {
            Self {
                native_creates,
                marker: PhantomData,
            }
        }
    }

    impl<F: 'static + Debug, T: 'static + Debug> EglSurfaceFactory for FakeFactory<F, T> {
        fn backend_id(&self) -> GraphicsBackendId {
            GraphicsBackendId::of::<F>()
        }

        fn platform_identity(&self) -> PlatformIdentity {
            PlatformIdentity::new::<F>(self.backend_id(), 0)
        }

        fn prepare(&self, _surface: &dyn Surface) -> EngineResult<PreparedEglSurfaceRef> {
            Ok(Arc::new(FakePrepared::<T> {
                native_creates: Arc::clone(&self.native_creates),
                marker: PhantomData,
            }))
        }
    }

    #[derive(Debug)]
    struct FakePrepared<T> {
        native_creates: Arc<AtomicUsize>,
        marker: PhantomData<fn() -> T>,
    }

    impl<T: 'static + Debug> PreparedEglSurface for FakePrepared<T> {
        fn backend_id(&self) -> GraphicsBackendId {
            GraphicsBackendId::of::<T>()
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn same_native_surface(&self, other: &dyn PreparedEglSurface) -> bool {
            other.as_any().downcast_ref::<Self>().is_some()
        }

        fn create_window_surface(
            &self,
            _egl: &EglInstance,
            _display: egl::Display,
            _config: egl::Config,
        ) -> EngineResult<egl::Surface> {
            self.native_creates.fetch_add(1, Ordering::Relaxed);
            Err(EngineError::new(ErrorCode::RenderBackendError))
        }
    }

    #[derive(Debug)]
    struct TestSurface;

    impl Surface for TestSurface {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn size(&self) -> (u32, u32) {
            (1, 1)
        }
    }

    #[test]
    fn platform_identity_distinguishes_native_domain_and_instance() {
        let backend = GraphicsBackendId::of::<BackendA>();
        let original = PlatformIdentity::new::<DomainA>(backend, 0x1000);
        let same = PlatformIdentity::new::<DomainA>(backend, 0x1000);
        let other_instance = PlatformIdentity::new::<DomainA>(backend, 0x2000);
        let other_domain = PlatformIdentity::new::<DomainB>(backend, 0x1000);

        assert_eq!(original, same);
        assert_ne!(original, other_instance);
        assert_ne!(original, other_domain);
        assert_eq!(original.backend_id(), backend);
    }

    #[test]
    fn rejects_mismatched_provider_and_factory_identity() {
        let err = GraphicsPlatform::try_new(
            Arc::new(FakeProvider::<BackendA>::new()),
            Arc::new(FakeFactory::<BackendB, BackendB>::new(Arc::new(
                AtomicUsize::new(0),
            ))),
        )
        .unwrap_err();

        assert_eq!(err.code, ErrorCode::InvalidOperation);
    }

    #[test]
    fn accepts_matched_pair_and_exposes_provider_label() {
        let platform = GraphicsPlatform::try_new(
            Arc::new(FakeProvider::<BackendA>::new()),
            Arc::new(FakeFactory::<BackendA, BackendA>::new(Arc::new(
                AtomicUsize::new(0),
            ))),
        )
        .unwrap();

        assert_eq!(platform.backend_id(), GraphicsBackendId::of::<BackendA>());
        assert_eq!(
            platform.platform_identity(),
            PlatformIdentity::new::<BackendA>(GraphicsBackendId::of::<BackendA>(), 0)
        );
        assert!(platform.egl_provider().label().ends_with("BackendA"));
    }

    #[test]
    fn rejects_factory_output_from_another_backend_before_native_creation() {
        let native_creates = Arc::new(AtomicUsize::new(0));
        let platform = GraphicsPlatform::try_new(
            Arc::new(FakeProvider::<BackendA>::new()),
            Arc::new(FakeFactory::<BackendA, BackendB>::new(Arc::clone(
                &native_creates,
            ))),
        )
        .unwrap();

        let err = platform.prepare_surface(&TestSurface).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unsupported);
        assert_eq!(native_creates.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn cross_type_native_comparison_fails_closed() {
        let creates = Arc::new(AtomicUsize::new(0));
        let a = FakePrepared::<BackendA> {
            native_creates: Arc::clone(&creates),
            marker: PhantomData,
        };
        let b = FakePrepared::<BackendB> {
            native_creates: creates,
            marker: PhantomData,
        };

        assert!(!a.same_native_surface(&b));
    }
}
