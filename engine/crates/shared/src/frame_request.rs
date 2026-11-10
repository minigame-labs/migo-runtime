//! The one frame a session has asked of its host.
//!
//! Where the host paces frames from its display (Android's Choreographer, a C ABI host's
//! display link), the engine asks it for one frame at a time and the host answers each
//! request with a vsync. Two parts of the engine ask: the render thread, while it has work
//! for the next frame, and the JavaScript thread, when content awaits an animation frame.
//! They used to ask independently, and the render thread kept its own note of whether it had
//! asked. Two requests outstanding at once were two vsyncs for one display frame; and when
//! the render thread dropped the second as a repeat, it could drop the answer to the request
//! it had noted, which nothing would ever answer again -- the frame loop stopped, with
//! content waiting for a frame and the host waiting for a request.
//!
//! So the request is one shared fact: asked, or not. [`FrameRequest::arm`] asks the host
//! only when nothing is asked; [`FrameRequest::clear`] is called when a vsync arrives --
//! whatever the render thread then does with it -- and wherever the host drops what was
//! asked (it pauses, or loses its surface), so that the next arm asks again.

use std::sync::atomic::{AtomicBool, Ordering};

pub struct FrameRequest {
    asked: AtomicBool,
    ask_host: Box<dyn Fn() + Send + Sync>,
}

impl FrameRequest {
    /// A request that asks the host through `ask_host`, which schedules exactly one frame.
    pub fn new(ask_host: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            asked: AtomicBool::new(false),
            ask_host: Box::new(ask_host),
        }
    }

    /// Ask the host for the next frame, unless one is asked and not yet answered: one vsync
    /// serves every part of the engine that wants that frame.
    pub fn arm(&self) {
        if self
            .asked
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            (self.ask_host)();
        }
    }

    /// Nothing is asked any more: a vsync answered it, or the host will not.
    pub fn clear(&self) {
        self.asked.store(false, Ordering::Release);
    }

    /// Whether a frame is asked and not yet answered.
    pub fn is_asked(&self) -> bool {
        self.asked.load(Ordering::Acquire)
    }
}

impl std::fmt::Debug for FrameRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameRequest")
            .field("asked", &self.is_asked())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::FrameRequest;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn counted() -> (Arc<FrameRequest>, Arc<AtomicUsize>) {
        let asks = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&asks);
        let request = FrameRequest::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        (Arc::new(request), asks)
    }

    #[test]
    fn a_frame_is_asked_once_until_it_is_answered() {
        let (request, asks) = counted();
        request.arm();
        request.arm();
        assert_eq!(asks.load(Ordering::SeqCst), 1);
        assert!(request.is_asked());

        request.clear();
        assert!(!request.is_asked());
        request.arm();
        assert_eq!(asks.load(Ordering::SeqCst), 2);
    }

    /// The render thread and the JavaScript thread arm the same frame at once: the host is
    /// asked once, whichever wins.
    #[test]
    fn two_threads_arming_together_ask_the_host_once() {
        for _ in 0..1000 {
            let (request, asks) = counted();
            let start = Arc::new(Barrier::new(2));
            let other = {
                let (request, start) = (Arc::clone(&request), Arc::clone(&start));
                thread::spawn(move || {
                    start.wait();
                    request.arm();
                })
            };
            start.wait();
            request.arm();
            other.join().expect("the other arming thread");
            assert_eq!(asks.load(Ordering::SeqCst), 1);
        }
    }

    /// An answer that clears the request while the other thread arms: either the arm saw it
    /// asked and the clear then lets the next arm through, or the arm came after the clear and
    /// asked. Never a request the host was not asked for.
    #[test]
    fn a_clear_racing_an_arm_never_leaves_a_request_unasked() {
        for _ in 0..1000 {
            let (request, asks) = counted();
            request.arm();
            let start = Arc::new(Barrier::new(2));
            let other = {
                let (request, start) = (Arc::clone(&request), Arc::clone(&start));
                thread::spawn(move || {
                    start.wait();
                    request.arm();
                })
            };
            start.wait();
            request.clear();
            other.join().expect("the arming thread");
            let asked = asks.load(Ordering::SeqCst);
            assert!(
                (asked == 2) == request.is_asked(),
                "asked {asked} times and is_asked={}: a request is noted exactly when the host was asked for it",
                request.is_asked()
            );
        }
    }
}
