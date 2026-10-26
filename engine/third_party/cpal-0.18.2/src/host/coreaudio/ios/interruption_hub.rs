//! Process-wide interruption ordering. Native stream handlers always run before
//! public listeners, including listeners registered while there is no device.
//! None of this runs on the render callback. Reentrant notifications are queued
//! rather than calling user code under a registry/dispatcher lock.

use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, Weak,
    },
};

/// A process audio-session interruption, after native streams have handled it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterruptionEvent {
    Began,
    Ended { should_resume: bool },
}

type Handler = dyn Fn(InterruptionEvent) + Send + Sync;

/// Retains a subscription. Dropping it unregisters future events. An event
/// already being dispatched may retain its callback until dispatch completes.
pub struct InterruptionObserver {
    _handler: Arc<Handler>,
}

#[derive(Default)]
struct Registry {
    streams: Vec<Weak<Handler>>,
    listeners: Vec<Weak<Handler>>,
}

#[derive(Default)]
struct Dispatch {
    pending: VecDeque<InterruptionEvent>,
    running: bool,
}

#[derive(Default)]
pub(super) struct InterruptionHub {
    registry: Mutex<Registry>,
    dispatch: Mutex<Dispatch>,
    interrupted: AtomicBool,
}

impl InterruptionHub {
    pub(super) fn is_interrupted(&self) -> bool {
        self.interrupted.load(Ordering::Acquire)
    }

    pub(super) fn observe(
        &self,
        native_stream: bool,
        handler: impl Fn(InterruptionEvent) + Send + Sync + 'static,
    ) -> InterruptionObserver {
        let handler: Arc<Handler> = Arc::new(handler);
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let entries = if native_stream {
            &mut registry.streams
        } else {
            &mut registry.listeners
        };
        entries.retain(|entry| entry.strong_count() != 0);
        entries.push(Arc::downgrade(&handler));
        InterruptionObserver { _handler: handler }
    }

    fn handlers(&self) -> (Vec<Arc<Handler>>, Vec<Arc<Handler>>) {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        fn live(entries: &mut Vec<Weak<Handler>>) -> Vec<Arc<Handler>> {
            let mut handlers = Vec::with_capacity(entries.len());
            entries.retain(|entry| {
                if let Some(handler) = entry.upgrade() {
                    handlers.push(handler);
                    true
                } else {
                    false
                }
            });
            handlers
        }
        (live(&mut registry.streams), live(&mut registry.listeners))
    }

    pub(super) fn deliver(&self, event: InterruptionEvent) {
        {
            let mut dispatch = self.dispatch.lock().unwrap_or_else(|e| e.into_inner());
            dispatch.pending.push_back(event);
            if dispatch.running {
                return;
            }
            dispatch.running = true;
        }
        // Restore drain ownership if a Rust client panics. A later notification
        // can drain queued events; no registry/dispatch lock crosses user code.
        struct Drain<'a>(&'a Mutex<Dispatch>, bool);
        impl Drop for Drain<'_> {
            fn drop(&mut self) {
                if self.1 {
                    self.0.lock().unwrap_or_else(|e| e.into_inner()).running = false;
                }
            }
        }
        let mut drain = Drain(&self.dispatch, true);
        loop {
            let event = {
                let mut dispatch = self.dispatch.lock().unwrap_or_else(|e| e.into_inner());
                match dispatch.pending.pop_front() {
                    Some(event) => event,
                    None => {
                        dispatch.running = false;
                        drain.1 = false;
                        return;
                    }
                }
            };
            self.interrupted
                .store(event == InterruptionEvent::Began, Ordering::Release);
            let (streams, listeners) = self.handlers();
            for handler in streams.iter().chain(listeners.iter()) {
                handler(event);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[test]
    fn all_native_handlers_finish_before_any_public_listener() {
        let hub = InterruptionHub::default();
        let completed = Arc::new(AtomicUsize::new(0));
        // Public registration first must not change delivery order.
        let seen = completed.clone();
        let _listener = hub.observe(false, move |_| assert_eq!(seen.load(Ordering::SeqCst), 2));
        let mut guards = Vec::new();
        for _ in 0..2 {
            let completed = completed.clone();
            guards.push(hub.observe(true, move |_| {
                completed.fetch_add(1, Ordering::SeqCst);
            }));
        }
        hub.deliver(InterruptionEvent::Ended {
            should_resume: false,
        });
    }

    #[test]
    fn silent_sessions_still_receive_events_and_drop_unregisters() {
        let hub = InterruptionHub::default();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let observer = hub.observe(false, move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        hub.deliver(InterruptionEvent::Began);
        drop(observer);
        hub.deliver(InterruptionEvent::Ended {
            should_resume: true,
        });
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(hub.registry.lock().unwrap().listeners.is_empty());
    }

    #[test]
    fn reentrant_events_do_not_overtake_the_current_public_event() {
        let hub = Arc::new(InterruptionHub::default());
        let order = Arc::new(Mutex::new(Vec::new()));
        let native_order = order.clone();
        let _stream = hub.observe(true, move |event| {
            native_order.lock().unwrap().push((true, event))
        });
        let public_order = order.clone();
        let nested = hub.clone();
        let _listener = hub.observe(false, move |event| {
            public_order.lock().unwrap().push((false, event));
            if event == InterruptionEvent::Began {
                nested.deliver(InterruptionEvent::Ended {
                    should_resume: false,
                });
                // Register/drop while dispatching: no registry lock is held.
                drop(nested.observe(false, |_| {}));
            }
        });
        hub.deliver(InterruptionEvent::Began);
        assert_eq!(
            *order.lock().unwrap(),
            vec![
                (true, InterruptionEvent::Began),
                (false, InterruptionEvent::Began),
                (
                    true,
                    InterruptionEvent::Ended {
                        should_resume: false
                    }
                ),
                (
                    false,
                    InterruptionEvent::Ended {
                        should_resume: false
                    }
                ),
            ]
        );
    }

    #[test]
    fn process_state_is_visible_before_listeners_and_survives_device_free_begin() {
        let hub = Arc::new(InterruptionHub::default());
        assert!(!hub.is_interrupted());
        hub.deliver(InterruptionEvent::Began);
        assert!(hub.is_interrupted());
        let observed = hub.clone();
        let _listener = hub.observe(false, move |event| {
            assert_eq!(observed.is_interrupted(), event == InterruptionEvent::Began);
        });
        hub.deliver(InterruptionEvent::Ended {
            should_resume: false,
        });
        assert!(!hub.is_interrupted());
    }

    #[test]
    fn competing_threads_cannot_overlap_native_and_public_dispatch() {
        let hub = Arc::new(InterruptionHub::default());
        let active = Arc::new(AtomicBool::new(false));
        let entered = active.clone();
        let _native = hub.observe(true, move |_| {
            assert!(!entered.swap(true, Ordering::SeqCst));
            std::thread::yield_now();
        });
        let exited = active.clone();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let _listener = hub.observe(false, move |_| {
            assert!(exited.swap(false, Ordering::SeqCst));
            seen.fetch_add(1, Ordering::SeqCst);
        });
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let hub = hub.clone();
                scope.spawn(move || {
                    for _ in 0..256 {
                        hub.deliver(InterruptionEvent::Began);
                    }
                });
            }
        });
        assert_eq!(count.load(Ordering::SeqCst), 1024);
        assert!(!active.load(Ordering::SeqCst));
    }
}
