//! `onDeviceMotionChange`: the angles every host reports in degrees reach content in the reference's unit, radians.

use super::support::{boot_with_services, run_to_idle};

#[test]
fn content_hears_the_angles_in_radians() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let mut runtime = boot_with_services(None, None);
    let heard = run_to_idle(
        &mut runtime,
        "globalThis.heard = null; \
         migo.onDeviceMotionChange((res) => { globalThis.heard = res; }); \
         globalThis[Symbol.for('Migo.hostBridge')]._internalTriggerDeviceMotionChange(90, -45, 30);",
        "JSON.stringify([heard.alpha, heard.beta, heard.gamma].map((v) => Math.round(v * 1e6) / 1e6))",
    );
    let expected = [
        std::f64::consts::FRAC_PI_2,
        -std::f64::consts::FRAC_PI_4,
        std::f64::consts::FRAC_PI_6,
    ]
    .map(|v| (v * 1e6).round() / 1e6);
    assert_eq!(
        heard,
        format!("[{},{},{}]", expected[0], expected[1], expected[2])
    );
}
