//! Monitors AVAudioSession lifecycle events, recovering the stream from an interruption and
//! reporting the rest as stream errors.

use std::{
    ptr::NonNull,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
};

use block2::RcBlock;
use objc2_avf_audio::{
    AVAudioSessionInterruptionNotification, AVAudioSessionInterruptionOptionKey,
    AVAudioSessionInterruptionOptions, AVAudioSessionInterruptionType,
    AVAudioSessionInterruptionTypeKey, AVAudioSessionMediaServicesWereLostNotification,
    AVAudioSessionMediaServicesWereResetNotification, AVAudioSessionRouteChangeNotification,
    AVAudioSessionRouteChangeReason, AVAudioSessionRouteChangeReasonKey,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSNumber, NSString};

use super::interruption_hub::{InterruptionEvent, InterruptionHub, InterruptionObserver};
use super::{input_latency_frames, output_latency_frames, StreamInner};
use crate::{
    host::{emit_error, latch::Latch, ErrorCallbackArc},
    Error, ErrorKind,
};

/// Runs `f` against the stream, if it is still alive and its lock is intact.
fn with_stream(stream: &Weak<Mutex<StreamInner>>, f: impl FnOnce(&mut StreamInner)) {
    if let Some(inner) = stream.upgrade() {
        if let Ok(mut inner) = inner.lock() {
            f(&mut inner);
        }
    }
}

/// Reads the number stored under `key` in a notification's `userInfo`.
fn user_info_number(notification: &NSNotification, key: Option<&NSString>) -> Option<usize> {
    let user_info = notification.userInfo()?;
    let value = user_info.objectForKey(key?)?;
    Some(value.downcast::<NSNumber>().ok()?.unsignedIntegerValue())
}

/// Shared buffer-depth value to refresh on route changes, paired with `is_input` to select the
/// input or output latency. `true` means an input stream.
type LatencyRefresh = (Arc<AtomicUsize>, bool);

fn route_change_error(notification: &NSNotification) -> Option<Error> {
    let key = unsafe { AVAudioSessionRouteChangeReasonKey };
    let reason = AVAudioSessionRouteChangeReason(user_info_number(notification, key)?);
    match reason {
        AVAudioSessionRouteChangeReason::OldDeviceUnavailable => Some(Error::with_message(
            ErrorKind::DeviceChanged,
            "Audio route changed",
        )),

        AVAudioSessionRouteChangeReason::CategoryChange
        | AVAudioSessionRouteChangeReason::Override
        | AVAudioSessionRouteChangeReason::RouteConfigurationChange => Some(Error::with_message(
            ErrorKind::StreamInvalidated,
            "Audio route changed",
        )),

        AVAudioSessionRouteChangeReason::NoSuitableRouteForCategory => Some(Error::with_message(
            ErrorKind::DeviceNotAvailable,
            "No suitable audio route for the session category",
        )),

        _ => None,
    }
}

pub(super) fn interruption_hub() -> &'static InterruptionHub {
    static HUB: OnceLock<InterruptionHub> = OnceLock::new();
    HUB.get_or_init(InterruptionHub::default)
}

fn install_interruption_observer() {
    // Process-lifetime registration, shared by native streams and device-free
    // subscribers. Store the opaque token exactly as the per-stream manager
    // does; NSNotificationCenter registration/removal is thread-safe on iOS.
    struct Observer {
        _token: objc2::rc::Retained<
            objc2::runtime::ProtocolObject<dyn objc2::runtime::NSObjectProtocol>,
        >,
    }
    unsafe impl Send for Observer {}
    unsafe impl Sync for Observer {}
    static OBSERVER: OnceLock<Option<Observer>> = OnceLock::new();
    OBSERVER.get_or_init(|| {
        let block = RcBlock::new(move |notif: NonNull<NSNotification>| {
            let notif = unsafe { notif.as_ref() };
            let Some(kind) = user_info_number(notif, unsafe { AVAudioSessionInterruptionTypeKey })
            else {
                return;
            };
            let event = match AVAudioSessionInterruptionType(kind) {
                AVAudioSessionInterruptionType::Began => InterruptionEvent::Began,
                AVAudioSessionInterruptionType::Ended => {
                    let options = AVAudioSessionInterruptionOptions(
                        user_info_number(notif, unsafe { AVAudioSessionInterruptionOptionKey })
                            .unwrap_or(0),
                    );
                    InterruptionEvent::Ended {
                        should_resume: options
                            .contains(AVAudioSessionInterruptionOptions::ShouldResume),
                    }
                }
                _ => return,
            };
            interruption_hub().deliver(event);
        });
        unsafe { AVAudioSessionInterruptionNotification }.map(|name| Observer {
            _token: unsafe {
                NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                    Some(name),
                    None,
                    None,
                    &block,
                )
            },
        })
    });
}

/// Observe the process session after CPAL has applied each interruption to all
/// registered streams. Available with no open audio device. Keep the returned
/// guard for the subscription lifetime. Callbacks run outside registry/stream
/// locks and must not panic; they are never invoked by the render callback.
pub fn observe_audio_session_interruptions(
    handler: impl Fn(InterruptionEvent) + Send + Sync + 'static,
) -> InterruptionObserver {
    let observer = interruption_hub().observe(false, handler);
    install_interruption_observer();
    observer
}

pub(super) struct SessionEventManager {
    latch: Latch,
    _interruption: InterruptionObserver,
    observers: Vec<
        objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2::runtime::NSObjectProtocol>>,
    >,
}

// SAFETY: NSNotificationCenter is thread-safe on iOS. The observer tokens stored here are opaque
// handles used only to call removeObserver in Drop; no data is read or written through them.
unsafe impl Send for SessionEventManager {}
unsafe impl Sync for SessionEventManager {}

impl SessionEventManager {
    pub(super) fn new(
        error_callback: ErrorCallbackArc,
        latch: Latch,
        latency_refresh: Option<LatencyRefresh>,
        stream: Weak<Mutex<StreamInner>>,
    ) -> Self {
        let nc = NSNotificationCenter::defaultCenter();
        let mut observers = Vec::new();
        let waiter = latch.waiter();

        let w = waiter.clone();
        let cb = error_callback.clone();
        let interruption = interruption_hub().observe(true, move |event| {
            if !w.is_released() {
                return;
            }
            let mut result = Ok(());
            with_stream(&stream, |inner| match event {
                InterruptionEvent::Began => inner.stop_for_interruption(),
                InterruptionEvent::Ended { should_resume } => {
                    result = inner.end_interruption(should_resume)
                }
            });
            // Native state is committed before public listeners run. Client
            // error code is likewise never called with StreamInner locked.
            if let Err(error) = result {
                emit_error(&cb, error);
            }
        });
        install_interruption_observer();

        {
            let cb = error_callback.clone();
            let w = waiter.clone();
            let block = RcBlock::new(move |notif: NonNull<NSNotification>| {
                if w.is_released() {
                    // The route may have changed the active device; recompute the buffer depth so
                    // capture/playback timestamps track the new latency.
                    if let Some((frames, is_input)) = &latency_refresh {
                        let depth = if *is_input {
                            input_latency_frames()
                        } else {
                            output_latency_frames()
                        };
                        frames.store(depth, Ordering::Relaxed);
                    }
                    let notif = unsafe { notif.as_ref() };
                    if let Some(err) = route_change_error(notif) {
                        emit_error(&cb, err);
                    }
                }
            });
            if let Some(name) = unsafe { AVAudioSessionRouteChangeNotification } {
                let observer = unsafe {
                    nc.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
                };
                observers.push(observer);
            }
        }

        {
            let cb = error_callback.clone();
            let w = waiter.clone();
            let block = RcBlock::new(move |_: NonNull<NSNotification>| {
                if w.is_released() {
                    emit_error(
                        &cb,
                        Error::with_message(
                            ErrorKind::DeviceNotAvailable,
                            "Audio media services were lost",
                        ),
                    );
                }
            });
            if let Some(name) = unsafe { AVAudioSessionMediaServicesWereLostNotification } {
                let observer = unsafe {
                    nc.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
                };
                observers.push(observer);
            }
        }

        {
            let cb = error_callback.clone();
            let w = waiter;
            let block = RcBlock::new(move |_: NonNull<NSNotification>| {
                if w.is_released() {
                    emit_error(
                        &cb,
                        Error::with_message(
                            ErrorKind::StreamInvalidated,
                            "Audio media services were reset",
                        ),
                    );
                }
            });
            if let Some(name) = unsafe { AVAudioSessionMediaServicesWereResetNotification } {
                let observer = unsafe {
                    nc.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
                };
                observers.push(observer);
            }
        }

        Self {
            latch,
            _interruption: interruption,
            observers,
        }
    }

    pub(super) fn signal_ready(&self) {
        self.latch.release();
    }
}

impl Drop for SessionEventManager {
    fn drop(&mut self) {
        let nc = NSNotificationCenter::defaultCenter();
        for observer in &self.observers {
            unsafe { nc.removeObserver(observer.as_ref()) };
        }
    }
}
