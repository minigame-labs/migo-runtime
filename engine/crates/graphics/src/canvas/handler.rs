use shared::{
    error::{EngineError, EngineResult, ErrorCode},
    protocol::render_cmd::CanvasCmd,
};

use crate::{CanvasManager, canvas::BackingSizeOwner};

pub(crate) struct CanvasHandler;

impl CanvasHandler {
    pub(crate) fn new() -> Self {
        CanvasHandler
    }

    /// Handle a canvas command, returning the outcome to the render thread.
    ///
    /// `RecreateOnscreen` must be intercepted by the render thread so generation
    /// preflight runs before raw-handle extraction. If it is ever routed here,
    /// fail closed and complete its responder without touching the Surface.
    /// Other commands communicate results through `resp` and return `Ok` here.
    pub(crate) fn handle_command(
        &mut self,
        cm: &mut CanvasManager,
        cmd: CanvasCmd,
    ) -> EngineResult<()> {
        match cmd {
            CanvasCmd::CreateOffscreen {
                width,
                height,
                resp,
            } => {
                let res = cm.create_offscreen(width, height);
                let _ = resp.send(res);
            }

            CanvasCmd::RegisterOffscreen { id, width, height } => {
                if let Err(e) = cm.register_offscreen(id, width, height) {
                    tracing::warn!(
                        "CanvasCmd::RegisterOffscreen failed: id={:?}, {}x{}, err={}",
                        id,
                        width,
                        height,
                        e
                    );
                }
            }

            CanvasCmd::DestroyCanvas { id, resp } => {
                let res = cm.destroy_canvas(id);
                let _ = resp.send(res);
            }

            CanvasCmd::RecreateOnscreen { .. } => {
                // Unreachable in production: the render loop matches this variant before
                // it delegates here, because installing a Surface needs the binding and
                // the control plane that only the loop holds. Answering with an error
                // rather than ignoring it keeps that fact checkable -- a caller that ever
                // reaches this gets a refusal it can log, not a silent no-op.
                return Err(EngineError::new(ErrorCode::InvalidOperation)
                    .with_msg("RecreateOnscreen must be preflighted by the render thread"));
            }

            CanvasCmd::ResizeCanvas { id, w, h } => {
                // Only `canvas.width`/`height` reaches here, so this is the
                // content claiming its backing size.
                let _ = cm.resize_canvas(id, w, h, BackingSizeOwner::Content);
            }

            CanvasCmd::MakeCurrent { id, resp } => {
                let res = cm.make_current_needed(id);
                let _ = resp.send(res);
            }

            CanvasCmd::SwapBuffers {
                id,
                wait_for_vsync,
                resp,
            } => {
                let res = cm.swap_buffers_no_restore(id, wait_for_vsync).map(|_| ());
                let _ = resp.send(res);
            }

            CanvasCmd::GetInfo { id, resp } => {
                let size = cm.get_canvas_size(id);
                let _ = resp.send(size);
            }

            CanvasCmd::LoadImage {
                image_id,
                image,
                priority,
                resp,
            } => {
                use shared::protocol::io_cmd::{DecodedImage, ImagePriority};
                match image {
                    DecodedImage::Compressed(compressed) => {
                        // GPU-direct compressed upload — always sync (fast, no PBO needed).
                        let res = cm.load_compressed_image(image_id, &compressed);
                        let _ = resp.send(res);
                    }
                    DecodedImage::HardwareBuffer(ahb_image) => {
                        // Zero-copy: the decoder wrote directly into
                        // this AHB; we hand it to EGLImage without
                        // another memcpy. `load_ahb_image` itself
                        // contains the fallback to a CPU round-trip
                        // when the device lacks AHB support.
                        //
                        // Priority is effectively "always critical"
                        // because the AHB path is already the
                        // lowest-latency option we have — async
                        // upload via PBO can't beat a direct
                        // `glEGLImageTargetTexture2DOES`.
                        let _ = priority;
                        let res = cm.load_ahb_image(image_id, ahb_image);
                        let _ = resp.send(res);
                    }
                    DecodedImage::Rgba(rgba_image) => {
                        // For Critical priority: always sync upload (don't defer).
                        // For Normal/Background: try async upload thread.
                        // On budget rejection (healthy upload thread,
                        // temporary squeeze) the request is deferred
                        // to next frame instead of falling back to a
                        // synchronous `glTexImage2D` on the render
                        // thread -- the sync path was the exact
                        // frame spike the async thread was meant to
                        // avoid.  The sync fallback is reserved for
                        // cases where waiting can't help: permanent
                        // degradation (no upload thread, or upload
                        // thread reported unrecoverable failure) and
                        // images that can never fit the async upload
                        // budget window.
                        if priority == ImagePriority::Critical {
                            let res = cm.load_shared_image(image_id, rgba_image);
                            let _ = resp.send(res);
                        } else {
                            match cm.submit_async_upload(image_id, &rgba_image, resp) {
                                Ok(()) => {}
                                Err(resp) => {
                                    match cm.async_upload_reject_action(rgba_image.rgba.len()) {
                                        crate::canvas::manager::AsyncUploadRejectAction::SyncFallback => {
                                            let res = cm.load_shared_image(image_id, rgba_image);
                                            let _ = resp.send(res);
                                        }
                                        crate::canvas::manager::AsyncUploadRejectAction::DeferRetry => {
                                            // Budget squeeze: queue for retry.
                                            // If the deferred queue is
                                            // already full (pathological
                                            // burst), last-resort sync.
                                            match cm.defer_upload(
                                                image_id,
                                                rgba_image.clone(),
                                                resp,
                                            ) {
                                                Ok(()) => {}
                                                Err((img, resp)) => {
                                                    let res = cm.load_shared_image(image_id, img);
                                                    let _ = resp.send(res);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            CanvasCmd::DestroyImage { image_id } => {
                cm.cancel_pending_load(image_id);
                let _ = cm.destroy_shared_image(image_id);
            }

            CanvasCmd::DestroyImages { image_ids } => {
                for image_id in image_ids {
                    cm.cancel_pending_load(image_id);
                    let _ = cm.destroy_shared_image(image_id);
                }
            }

            _ => {}
        }
        Ok(())
    }
}
