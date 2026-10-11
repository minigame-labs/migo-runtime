//! A gamepad's `vibrationActuator`: the W3C Gamepad API's GamepadHapticActuator,
//! timed by the engine. What the host's motors are told, and what content's
//! promises settle with.

use std::sync::{Arc, Mutex};

use deno_core::JsRuntime;
use shared::{
    protocol::error::ServiceError,
    services::{
        CommerceServices, ConnectivityServices, GamepadRumble, GamepadService, MediaServices,
        SensorServices, SystemUtilServices,
    },
};

use super::support::{boot_with_services, run_to_idle};

/// The host's motors, as the instructions they were given.
#[derive(Default)]
struct Motors(Mutex<Vec<String>>);

impl GamepadService for Motors {
    fn rumble(&self, rumble: &GamepadRumble) -> Result<(), ServiceError> {
        self.0.lock().unwrap().push(format!(
            "pad{} {}ms {} {} {} {}",
            rumble.index,
            rumble.duration_ms,
            rumble.strong_magnitude,
            rumble.weak_magnitude,
            rumble.left_trigger,
            rumble.right_trigger
        ));
        Ok(())
    }
}

struct Host(Option<Arc<Motors>>);

impl SensorServices for Host {}
impl MediaServices for Host {}
impl ConnectivityServices for Host {}
impl CommerceServices for Host {}
impl SystemUtilServices for Host {
    fn gamepad(&self) -> Option<Arc<dyn GamepadService>> {
        self.0
            .clone()
            .map(|motors| motors as Arc<dyn GamepadService>)
    }
}

/// A runtime with pad 0 (dual rumble), pad 1 (dual and trigger rumble) and pad
/// 2 (no motors) connected, and `__record(name, promise)` putting how each
/// promise settled in `__settled[name]`.
fn boot(motors: Option<Arc<Motors>>) -> JsRuntime {
    let mut runtime = boot_with_services(Some(Arc::new(Host(motors))), None);
    run_to_idle(
        &mut runtime,
        r#"
        globalThis.__bridge = globalThis[Symbol.for('Migo.hostBridge')];
        __bridge._internalTriggerGamepadConnected(0, 'dual', 'standard', 4, 17, 1);
        __bridge._internalTriggerGamepadConnected(1, 'triggers', 'standard', 4, 17, 3);
        __bridge._internalTriggerGamepadConnected(2, 'still', 'standard', 4, 17, 0);
        globalThis.__settled = {};
        globalThis.__record = function (name, promise) {
            promise.then(
                function (result) { __settled[name] = result; },
                function (error) { __settled[name] = error.name; });
        };
        "#,
        "0",
    );
    runtime
}

fn heard(motors: &Motors) -> Vec<String> {
    std::mem::take(&mut *motors.0.lock().unwrap())
}

fn settled(runtime: &mut JsRuntime, script: &str) -> String {
    run_to_idle(runtime, script, "JSON.stringify(__settled)")
}

#[test]
fn every_pad_has_one_actuator_listing_the_effects_its_host_declared() {
    let mut runtime = boot(Some(Arc::default()));
    let shape = run_to_idle(
        &mut runtime,
        "",
        r#"
        const pads = migo.getGamepads();
        JSON.stringify([
            pads.map(function (pad) { return pad.vibrationActuator.effects; }),
            pads[0].vibrationActuator === migo.getGamepads()[0].vibrationActuator,
            Object.isFrozen(pads[1].vibrationActuator.effects),
            typeof pads[2].vibrationActuator.playEffect,
        ])
        "#,
    );
    assert_eq!(
        shape,
        r#"[[["dual-rumble"],["dual-rumble","trigger-rumble"],[]],true,true,"function"]"#
    );
}

#[test]
fn an_effect_plays_on_the_host_s_motors_and_completes_when_its_time_is_up() {
    let motors = Arc::new(Motors::default());
    let mut runtime = boot(Some(Arc::clone(&motors)));
    let outcome = settled(
        &mut runtime,
        r#"
        const pads = migo.getGamepads();
        __record('dual', pads[0].vibrationActuator.playEffect('dual-rumble',
            { duration: 20, strongMagnitude: 1, weakMagnitude: 0.5,
              leftTrigger: 1, rightTrigger: 1 }));
        __record('trigger', pads[1].vibrationActuator.playEffect('trigger-rumble',
            { duration: 20, strongMagnitude: 0.25, leftTrigger: 0.75, rightTrigger: 1 }));
        "#,
    );
    assert_eq!(outcome, r#"{"dual":"complete","trigger":"complete"}"#);
    // A dual-rumble effect has no trigger motors to drive, whatever it was given;
    // nothing tells the motors to stop, because the duration already did.
    assert_eq!(
        heard(&motors),
        ["pad0 20ms 1 0.5 0 0", "pad1 20ms 0.25 0 0.75 1"]
    );
}

#[test]
fn the_next_effect_preempts_the_one_playing_and_reset_stops_it() {
    let motors = Arc::new(Motors::default());
    let mut runtime = boot(Some(Arc::clone(&motors)));
    let outcome = settled(
        &mut runtime,
        r#"
        const actuator = migo.getGamepads()[0].vibrationActuator;
        __record('first', actuator.playEffect('dual-rumble', { duration: 4000, strongMagnitude: 1 }));
        __record('second', actuator.playEffect('dual-rumble', { duration: 4000, weakMagnitude: 1 }));
        __record('reset', actuator.reset());
        __record('idle reset', actuator.reset());
        "#,
    );
    assert_eq!(
        outcome,
        r#"{"first":"preempted","second":"preempted","reset":"complete","idle reset":"complete"}"#
    );
    // Each effect replaces the last on the pad, so only reset needs a stop --
    // and a reset with nothing playing has nothing to stop.
    assert_eq!(
        heard(&motors),
        [
            "pad0 4000ms 1 0 0 0",
            "pad0 4000ms 0 1 0 0",
            "pad0 0ms 0 0 0 0"
        ]
    );
}

#[test]
fn a_start_delay_is_silent_and_the_whole_effect_fits_the_limit() {
    let motors = Arc::new(Motors::default());
    let mut runtime = boot(Some(Arc::clone(&motors)));
    let outcome = settled(
        &mut runtime,
        r#"
        const actuator = migo.getGamepads()[0].vibrationActuator;
        __record('running', actuator.playEffect('dual-rumble', { duration: 4000, strongMagnitude: 1 }));
        __record('delayed', actuator.playEffect('dual-rumble',
            { startDelay: 15, duration: 10, strongMagnitude: 0.5 }));
        "#,
    );
    assert_eq!(outcome, r#"{"running":"preempted","delayed":"complete"}"#);
    // The running effect stops for the delay; the delayed one starts after it.
    assert_eq!(
        heard(&motors),
        [
            "pad0 4000ms 1 0 0 0",
            "pad0 0ms 0 0 0 0",
            "pad0 10ms 0.5 0 0 0"
        ]
    );

    let outcome = settled(
        &mut runtime,
        r#"{
        const actuator = migo.getGamepads()[0].vibrationActuator;
        __record('long', actuator.playEffect('dual-rumble', { duration: 60000, strongMagnitude: 1 }));
        __record('negative', actuator.playEffect('dual-rumble', { duration: -1, weakMagnitude: 1 }));
        actuator.reset();
        }"#,
    );
    assert_eq!(
        outcome,
        r#"{"running":"preempted","delayed":"complete","long":"preempted","negative":"preempted"}"#
    );
    // WebIDL wraps a negative unsigned long long past any limit, which it then meets.
    assert_eq!(
        heard(&motors),
        [
            "pad0 5000ms 1 0 0 0",
            "pad0 5000ms 0 1 0 0",
            "pad0 0ms 0 0 0 0"
        ]
    );
}

#[test]
fn invalid_effects_reject_as_the_standard_says_and_reach_no_motor() {
    let motors = Arc::new(Motors::default());
    let mut runtime = boot(Some(Arc::clone(&motors)));
    let outcome = settled(
        &mut runtime,
        r#"
        const pads = migo.getGamepads();
        const dual = pads[0].vibrationActuator;
        __record('type', dual.playEffect('buzz', {}));
        __record('magnitude', dual.playEffect('dual-rumble', { strongMagnitude: 1.5 }));
        __record('nan', dual.playEffect('dual-rumble', { weakMagnitude: NaN }));
        __record('params', dual.playEffect('dual-rumble', 7));
        __record('trigger magnitude',
            pads[1].vibrationActuator.playEffect('trigger-rumble', { rightTrigger: -0.1 }));
        __record('no motors', pads[2].vibrationActuator.playEffect('dual-rumble', { duration: 10 }));
        __record('playing', dual.playEffect('dual-rumble', { duration: 4000, strongMagnitude: 1 }));
        __record('unsupported', dual.playEffect('trigger-rumble', { duration: 10 }));
        "#,
    );
    assert_eq!(
        outcome,
        concat!(
            r#"{"type":"TypeError","magnitude":"TypeError","nan":"TypeError","params":"TypeError","#,
            r#""trigger magnitude":"TypeError","no motors":"NotSupportedError","playing":"preempted","#,
            r#""unsupported":"NotSupportedError"}"#
        )
    );
    // The standard preempts what plays before it finds the type unsupported;
    // preempted means stopped, so the motors are told.
    assert_eq!(heard(&motors), ["pad0 4000ms 1 0 0 0", "pad0 0ms 0 0 0 0"]);
}

#[test]
fn a_silent_effect_reaches_no_motor_that_is_already_still() {
    let motors = Arc::new(Motors::default());
    let mut runtime = boot(Some(Arc::clone(&motors)));
    let outcome = settled(
        &mut runtime,
        r#"
        const actuator = migo.getGamepads()[0].vibrationActuator;
        __record('silent', actuator.playEffect('dual-rumble', { duration: 10 }));
        __record('instant', actuator.playEffect('dual-rumble', { strongMagnitude: 1 }));
        "#,
    );
    assert_eq!(outcome, r#"{"silent":"preempted","instant":"complete"}"#);
    assert!(heard(&motors).is_empty());
}

#[test]
fn going_to_the_background_preempts_and_stops_and_effects_wait_for_the_foreground() {
    let motors = Arc::new(Motors::default());
    let mut runtime = boot(Some(Arc::clone(&motors)));
    let outcome = settled(
        &mut runtime,
        r#"
        // Content clearing every hide listener it has must not reach the engine's.
        migo.offHide();
        const actuator = migo.getGamepads()[0].vibrationActuator;
        __record('playing', actuator.playEffect('dual-rumble', { duration: 4000, strongMagnitude: 1 }));
        __bridge._internalTriggerOnHide();
        __record('hidden play', actuator.playEffect('dual-rumble', { duration: 10, strongMagnitude: 1 }));
        __record('hidden reset', actuator.reset());
        __bridge._internalTriggerOnShow({});
        __record('shown', actuator.playEffect('dual-rumble', { duration: 10, weakMagnitude: 1 }));
        "#,
    );
    assert_eq!(
        outcome,
        concat!(
            r#"{"playing":"preempted","hidden play":"InvalidStateError","#,
            r#""hidden reset":"InvalidStateError","shown":"complete"}"#
        )
    );
    assert_eq!(
        heard(&motors),
        [
            "pad0 4000ms 1 0 0 0",
            "pad0 0ms 0 0 0 0",
            "pad0 10ms 0 1 0 0"
        ]
    );
}

#[test]
fn a_pad_that_goes_away_ends_its_effect_and_its_actuator_plays_no_more() {
    let motors = Arc::new(Motors::default());
    let mut runtime = boot(Some(Arc::clone(&motors)));
    let outcome = settled(
        &mut runtime,
        r#"
        const actuator = migo.getGamepads()[0].vibrationActuator;
        __record('playing', actuator.playEffect('dual-rumble', { duration: 4000, strongMagnitude: 1 }));
        __bridge._internalTriggerGamepadDisconnected(0);
        __record('gone', actuator.playEffect('dual-rumble', { duration: 10, strongMagnitude: 1 }));
        __record('gone reset', actuator.reset());
        "#,
    );
    assert_eq!(
        outcome,
        r#"{"playing":"preempted","gone":"NotSupportedError","gone reset":"complete"}"#
    );
    // The motors went with the pad: nothing is told to stop.
    assert_eq!(heard(&motors), ["pad0 4000ms 1 0 0 0"]);
}

#[test]
fn an_effect_the_host_cannot_take_rejects() {
    let mut runtime = boot(None);
    let outcome = settled(
        &mut runtime,
        r#"
        const actuator = migo.getGamepads()[0].vibrationActuator;
        __record('now', actuator.playEffect('dual-rumble', { duration: 10, strongMagnitude: 1 }));
        __record('later', actuator.playEffect('dual-rumble',
            { startDelay: 5, duration: 10, strongMagnitude: 1 }));
        "#,
    );
    assert_eq!(
        outcome,
        r#"{"now":"OperationError","later":"OperationError"}"#
    );
}
