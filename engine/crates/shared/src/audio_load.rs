//! A source load keeps its original owner alive across preparation and queues.
//! The owner remains the audio player's existing allocation; this erased view
//! crosses the service boundary without retaining PCM or allocating a token box.
use std::{
    future::{Future, poll_fn},
    sync::Arc,
    task::Poll,
};
use tokio::sync::Notify;

use crate::error::{EngineError, EngineResult, ErrorCode};

pub trait AudioLoadState: Send + Sync {
    fn generation(&self) -> u64;
    fn generation_changed(&self) -> &Notify;
}

/// One completion capability. It is moved, never cloned, into a load command.
pub struct InnerAudioLoadToken {
    watch: AudioLoadWatch,
    source_id: u32,
}

/// Cancellation observation retains only atomics and notification state.
pub struct AudioLoadWatch {
    state: Arc<dyn AudioLoadState>,
    generation: u64,
}

impl std::fmt::Debug for InnerAudioLoadToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InnerAudioLoadToken")
            .field("generation", &self.watch.generation)
            .field("source_id", &self.source_id)
            .finish_non_exhaustive()
    }
}

impl InnerAudioLoadToken {
    pub fn new<S: AudioLoadState + 'static>(
        state: &Arc<S>,
        generation: u64,
        source_id: u32,
    ) -> Self {
        Self {
            watch: AudioLoadWatch {
                state: state.clone(),
                generation,
            },
            source_id,
        }
    }

    pub fn source_id(&self) -> u32 {
        self.source_id
    }

    pub fn is_current(&self) -> bool {
        self.watch.is_current()
    }

    pub fn matches<S: AudioLoadState>(&self, owner: &Arc<S>) -> bool {
        std::ptr::addr_eq(Arc::as_ptr(&self.watch.state), Arc::as_ptr(owner)) && self.is_current()
    }

    pub fn watch(&self) -> AudioLoadWatch {
        AudioLoadWatch {
            state: self.watch.state.clone(),
            generation: self.watch.generation,
        }
    }

    pub fn cancelled_error() -> EngineError {
        EngineError::from_detail(ErrorCode::Cancelled, "InnerAudio source load superseded")
    }
}

impl AudioLoadWatch {
    pub fn is_current(&self) -> bool {
        self.generation != 0 && self.state.generation() == self.generation
    }

    pub async fn while_current<F: Future>(&self, operation: F) -> EngineResult<F::Output> {
        let mut changed = std::pin::pin!(self.state.generation_changed().notified());
        changed.as_mut().enable();
        let mut operation = std::pin::pin!(operation);
        poll_fn(|cx| {
            // Cancellation wins when both branches are ready. Registration
            // precedes the generation check, so a change cannot lose its wake.
            if !self.is_current() || changed.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(InnerAudioLoadToken::cancelled_error()));
            }
            operation.as_mut().poll(cx).map(|result| {
                if self.is_current() {
                    Ok(result)
                } else {
                    Err(InnerAudioLoadToken::cancelled_error())
                }
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicU64, AtomicUsize, Ordering},
        task::{Context, Wake, Waker},
    };

    struct State {
        generation: AtomicU64,
        changed: Notify,
    }
    impl AudioLoadState for State {
        fn generation(&self) -> u64 {
            self.generation.load(Ordering::Acquire)
        }
        fn generation_changed(&self) -> &Notify {
            &self.changed
        }
    }
    fn state() -> Arc<State> {
        Arc::new(State {
            generation: AtomicU64::new(1),
            changed: Notify::new(),
        })
    }
    fn retire(state: &State) {
        state.generation.store(2, Ordering::Release);
        state.changed.notify_waiters();
    }
    struct DropMark(Arc<AtomicUsize>);
    impl Drop for DropMark {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }
    struct Wakes(AtomicUsize);
    impl Wake for Wakes {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn tokens_preserve_identity_and_source_tag_without_retaining_other_owners() {
        let first = state();
        let second = state();
        let token = InnerAudioLoadToken::new(&first, 1, 701);
        assert!(token.matches(&first));
        assert!(!token.matches(&second));
        assert_eq!(token.source_id(), 701);
        assert!(!InnerAudioLoadToken::new(&first, 0, 701).is_current());
        let zero = state();
        zero.generation.store(0, Ordering::Release);
        assert!(!InnerAudioLoadToken::new(&zero, 0, 701).is_current());
        let watch = token.watch();
        drop(token);
        assert!(watch.is_current());
        retire(&first);
        assert!(!watch.is_current());
    }

    #[test]
    fn a_retired_operation_is_never_polled_even_when_ready() {
        let state = state();
        let watch = InnerAudioLoadToken::new(&state, 1, 701).watch();
        retire(&state);
        let polled = AtomicUsize::new(0);
        let mut operation = std::pin::pin!(watch.while_current(async {
            polled.fetch_add(1, Ordering::AcqRel);
            42
        }));
        let result = operation
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()));
        assert!(matches!(result, Poll::Ready(Err(error)) if error.code == ErrorCode::Cancelled));
        assert_eq!(
            polled.load(Ordering::Acquire),
            0,
            "retired preparation was polled"
        );
    }

    #[test]
    fn a_changed_owner_wakes_and_drops_pending_preparation_without_repolling_it() {
        let state = state();
        let watch = InnerAudioLoadToken::new(&state, 1, 701).watch();
        let wakes = Arc::new(Wakes(AtomicUsize::new(0)));
        let waker = Waker::from(wakes.clone());
        let dropped = Arc::new(AtomicUsize::new(0));
        let mark = DropMark(dropped.clone());
        {
            let mut operation = std::pin::pin!(watch.while_current(async move {
                let _held = mark;
                std::future::pending::<()>().await;
            }));
            assert!(
                operation
                    .as_mut()
                    .poll(&mut Context::from_waker(&waker))
                    .is_pending()
            );
            assert_eq!(wakes.0.load(Ordering::Acquire), 0);
            retire(&state);
            assert_eq!(
                wakes.0.load(Ordering::Acquire),
                1,
                "wake must not need a waiter repoll"
            );
            let result = operation.as_mut().poll(&mut Context::from_waker(&waker));
            assert!(
                matches!(result, Poll::Ready(Err(error)) if error.code == ErrorCode::Cancelled)
            );
        }
        assert_eq!(dropped.load(Ordering::Acquire), 1);
    }

    #[test]
    fn cancellation_during_a_ready_step_drops_its_unpublished_result() {
        let state = state();
        let watch = InnerAudioLoadToken::new(&state, 1, 701).watch();
        let dropped = Arc::new(AtomicUsize::new(0));
        {
            let result = DropMark(dropped.clone());
            let mut operation = std::pin::pin!(watch.while_current(async {
                retire(&state);
                result
            }));
            let result = operation
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()));
            assert!(
                matches!(result, Poll::Ready(Err(error)) if error.code == ErrorCode::Cancelled)
            );
        }
        assert_eq!(dropped.load(Ordering::Acquire), 1);
    }

    #[test]
    fn source_observation_and_ready_waits_have_no_steady_heap_activity() {
        use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
        let state = state();
        assert_no_steady_state_allocation(
            Burst {
                path: "audio source watch and ready operation",
                warmup: 4,
                measured: 128,
            },
            |_| {
                let token = InnerAudioLoadToken::new(&state, 1, 701);
                let watch = token.watch();
                let mut operation = std::pin::pin!(watch.while_current(std::future::ready(42)));
                assert!(matches!(
                    operation
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop())),
                    Poll::Ready(Ok(42))
                ));
            },
        );
    }
}
