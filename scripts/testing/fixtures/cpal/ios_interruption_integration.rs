#[test]
fn game_end_callback_can_resume_immediately_after_native_denial() {
    use interruption_hub::{InterruptionEvent, InterruptionHub};
    use playback_state::PlaybackState;
    use std::sync::{Arc, Mutex};

    let hub = InterruptionHub::default();
    let state = Arc::new(Mutex::new(PlaybackState::default()));
    state.lock().unwrap().play(|| Ok::<_, ()>(())).unwrap();
    let native_state = state.clone();
    // Deliberately register the game first: order must follow ownership.
    let game_state = state.clone();
    let _game = hub.observe(false, move |event| {
        if let InterruptionEvent::Ended { should_resume: false } = event {
            let mut state = game_state.lock().unwrap();
            assert!(state.requires_resume(), "game end arrived before native end");
            state.play(|| Ok::<_, ()>(())).unwrap();
        }
    });
    let _native = hub.observe(true, move |event| {
        let mut state = native_state.lock().unwrap();
        match event {
            InterruptionEvent::Began => state.begin(|| {}),
            InterruptionEvent::Ended { should_resume } => state.end(should_resume, || Ok::<_, ()>(())).unwrap(),
        }
    });
    hub.deliver(InterruptionEvent::Began);
    hub.deliver(InterruptionEvent::Ended { should_resume: false });
    assert!(!state.lock().unwrap().requires_resume());
}
