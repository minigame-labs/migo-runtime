use super::*;

#[test]
fn every_backend_replacement_revokes_the_previous_route_even_when_format_repeats() {
    let mut contexts = HashMap::new();
    let mut context = AudioContext::new(1, 44100, 1);
    context
        .process_native_device(&mut [0.0; 1], 44100, 1)
        .unwrap();
    let old = context.device_epoch();
    context.native_input.push(99.0);
    context.native_collected.push(88);
    contexts.insert(1, context);
    assert!(!crate::audio_thread::renegotiate_context_routes(
        &mut contexts,
        (48000, 1),
        (48000, 1)
    ));
    assert!(
        !old.is_live(),
        "same-format backend replacement kept the old route live"
    );
    let context = contexts.get_mut(&1).unwrap();
    assert!(
        context.native_input.is_empty() && context.native_collected.is_empty(),
        "backend replacement retained old PCM or phase"
    );
    let next = context.device_epoch();
    assert!(!next.same(&old));
    context
        .process_native_device(&mut [0.0; 128], 44100, 1)
        .unwrap();
    assert!(
        next.is_live() && next.same(&context.device_epoch()),
        "initial render invalidated an already bound route"
    );
    assert!(crate::audio_thread::renegotiate_context_routes(
        &mut contexts,
        (48000, 1),
        (96000, 2)
    ));
    assert!(
        !next.is_live(),
        "changed-format backend replacement kept the old route live"
    );
}
#[test]
fn backend_rate_and_channel_changes_have_new_epochs_without_aba_revival() {
    let context = AudioContext::new(1, 44100, 2);
    let initial = context.device_epoch();
    let mut contexts = HashMap::from([(1, context)]);
    crate::audio_thread::renegotiate_context_routes(&mut contexts, (48000, 2), (96000, 2));
    assert!(!initial.is_live(), "rate change retained its route epoch");
    let second = contexts[&1].device_epoch();
    crate::audio_thread::renegotiate_context_routes(&mut contexts, (96000, 2), (96000, 1));
    assert!(!second.is_live(), "channel change retained its route epoch");
    let third = contexts[&1].device_epoch();
    crate::audio_thread::renegotiate_context_routes(&mut contexts, (96000, 1), (48000, 2));
    assert!(!third.is_live());
    assert!(
        !initial.same(&contexts[&1].device_epoch()),
        "A-B-A revived an earlier route"
    );
}
#[test]
fn suspend_preserves_identity_but_close_drop_and_id_reuse_revoke_it() {
    let mut context = AudioContext::new(7, 44100, 2);
    let epoch = context.device_epoch();
    context.suspend();
    context.resume();
    assert!(
        epoch.is_live() && epoch.same(&context.device_epoch()),
        "suspension discarded a live route"
    );
    context.close();
    assert!(!epoch.is_live(), "context close retained route authority");
    context.renegotiate_device(48000, 2);
    assert!(
        !context.device_epoch().is_live(),
        "closed context revived route authority"
    );
    let second = AudioContext::new(7, 44100, 2);
    let current = second.device_epoch();
    assert!(
        !epoch.same(&current),
        "numeric context reuse revived old authority"
    );
    drop(second);
    assert!(!current.is_live(), "context drop retained route authority");
}
#[test]
fn route_epochs_do_not_allocate_on_steady_device_quanta() {
    let mut context = AudioContext::new(7, 44100, 2);
    let mut output = [0.0; 256];
    context
        .process_native_device(&mut output, 44100, 2)
        .unwrap();
    let epoch = context.device_epoch();
    let before = migo_alloc_probe::thread_counts();
    for _ in 0..512 {
        context
            .process_native_device(&mut output, 44100, 2)
            .unwrap();
    }
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        before.allocations, after.allocations,
        "steady route identity allocated"
    );
    assert_eq!(before.reallocations, after.reallocations);
    assert!(epoch.same(&context.device_epoch()));
}
