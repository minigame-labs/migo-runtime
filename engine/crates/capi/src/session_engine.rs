//! Which execution a session runs, decided at compile time.
//!
//! A Migo session either owns a JavaScript runtime in this process or renders
//! frames produced by one somewhere else. The two are mutually exclusive per
//! product -- `migo-core` refuses to compile both -- so the choice is a `cfg`
//! rather than an enum: an enum whose second variant no build can construct is
//! a runtime representation of a compile-time fact, and it would cost a
//! discriminant, a match, and dead code in whichever product is not using it.
//!
//! What this module exists to do is keep the C boundary from having to know.
//! `MigoSession` holds a [`SessionEngine`], starts one with [`spawn_tracked`],
//! and asks it for an id; those three things are the same sentence in both
//! products, and everything below that line differs.

use std::sync::Arc;

use shared::{
    config::InitOptions,
    error::EngineResult,
    surface::{PublicSurfaceGeneration, SurfaceRef, SurfaceResourceLease},
};

use migo_core::PlatformServices;

/// The running session, whichever execution this product compiled.
#[cfg(feature = "embedded-v8")]
pub(crate) type SessionEngine = migo_core::HostThread;
#[cfg(feature = "external-frames")]
pub(crate) type SessionEngine = migo_core::ExternalFrameSession;

/// A started session and the lease for the public attachment handle it was
/// given. Both products hand one back, because the C boundary owns that
/// handle's lifetime either way.
pub(crate) struct StartedEngine {
    pub(crate) engine: SessionEngine,
    pub(crate) resource: SurfaceResourceLease,
    /// The session's own direct data-plane handles, from the registration that
    /// published them.
    ///
    /// Handed over rather than looked up. `attach` used to recover this by asking
    /// the process-global Host registry for the id it had just started -- and a
    /// session whose renderer failed removes its own entry on the way out, so the
    /// lookup was racing the teardown of the very thing it was installing.
    /// Measured on the iOS simulator with no ANGLE present, `attach` lost that race
    /// about two runs in three, answering MIGO_ERROR_INTERNAL where the other third
    /// answered MIGO_OK for one input. A host cannot program against that.
    pub(crate) ingress: migo_core::HostIngress,
}

/// Start a session against a Surface whose public generation the caller owns.
///
/// `launch_nonce` is the 128-bit identity an external producer's packets must
/// carry. The embedded execution has no producer to authenticate and ignores
/// it; the parameter is present in both signatures rather than behind a `cfg`
/// so the call site reads the same, and so that adding an external product
/// later cannot silently drop it.
pub(crate) fn spawn_tracked(
    surface: SurfaceRef,
    public_generation: PublicSurfaceGeneration,
    graphics_platform: graphics::egl_platform::GraphicsPlatform,
    platform: Arc<dyn PlatformServices>,
    options: InitOptions,
    #[cfg_attr(feature = "embedded-v8", allow(unused_variables))] launch_nonce: u128,
) -> EngineResult<StartedEngine> {
    #[cfg(feature = "embedded-v8")]
    {
        let started = migo_core::spawn_host_thread_tracked(
            surface,
            public_generation,
            graphics_platform,
            platform,
            options,
        )?;
        Ok(StartedEngine {
            engine: started.host,
            resource: started.resource,
            ingress: started.ingress,
        })
    }
    #[cfg(feature = "external-frames")]
    {
        let started = migo_core::spawn_external_frame_session(
            launch_nonce,
            Some(surface),
            Some(public_generation),
            graphics_platform,
            platform,
            options,
        )?;
        Ok(StartedEngine {
            engine: started.session,
            // Infallible by construction: this entry point always passes a
            // Surface, and the spawn mints a lease for exactly the Surfaces it
            // is given.
            resource: started
                .resource
                .expect("a tracked spawn passes a Surface, so a resource lease exists"),
            ingress: started.ingress,
        })
    }
}

/// A session around an already-running thread, for the boundary's own tests.
// `test` as well as the feature: this crate's own unit tests do not enable its
// public `test-support` feature, and the call sites they replace reached
// `HostThread::from_join_handle_for_test` through the dev-dependency instead.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn engine_for_test(
    host_id: migo_core::HostId,
    join: std::thread::JoinHandle<()>,
) -> SessionEngine {
    #[cfg(feature = "embedded-v8")]
    {
        migo_core::HostThread::from_join_handle_for_test(host_id, join)
    }
    #[cfg(feature = "external-frames")]
    {
        // A fixed nonce: these sessions never see a packet, and a test that
        // needed a real one would be testing the ingress rather than the
        // boundary.
        migo_core::ExternalFrameSession::from_join_handle_for_test(host_id, join, 1)
    }
}
