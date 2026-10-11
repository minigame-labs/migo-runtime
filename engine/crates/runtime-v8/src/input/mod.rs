use deno_core::{Extension, OpState, extension, op2};
use deno_error::JsErrorBox;
use shared::op_state::HostOpState;
use shared::services::GamepadRumble;

#[op2(fast)]
fn op_show_keyboard(state: &mut OpState, #[string] options_json: String) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(keyboard) = services.keyboard() {
            return keyboard.show(&options_json).map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("showKeyboard:fail not supported"))
}

#[op2(fast)]
fn op_hide_keyboard(state: &mut OpState) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(keyboard) = services.keyboard() {
            return keyboard.hide().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("hideKeyboard:fail not supported"))
}

#[op2(fast)]
fn op_update_keyboard(state: &mut OpState, #[string] value: String) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(keyboard) = services.keyboard() {
            return keyboard.update(&value).map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("updateKeyboard:fail not supported"))
}

/// Set a pad's motors for `vibrationActuator`: the engine has timed the
/// effect, so this is one instruction -- these magnitudes for this long -- and
/// a zero duration stops the pad.
#[op2(fast)]
fn op_gamepad_rumble(
    state: &mut OpState,
    index: u32,
    strong_magnitude: f64,
    weak_magnitude: f64,
    left_trigger: f64,
    right_trigger: f64,
    duration_ms: u32,
) -> Result<(), JsErrorBox> {
    let rumble = GamepadRumble::new(
        index,
        strong_magnitude,
        weak_magnitude,
        left_trigger,
        right_trigger,
        duration_ms,
    )
    .map_err(JsErrorBox::generic)?;
    let host = state.borrow::<HostOpState>();
    match host
        .device_services
        .as_ref()
        .and_then(|services| services.gamepad())
    {
        Some(gamepad) => gamepad.rumble(&rumble).map_err(JsErrorBox::generic),
        None => Err(JsErrorBox::generic("playEffect:fail not supported")),
    }
}

extension!(host_v8_touch,
ops = [op_show_keyboard, op_hide_keyboard, op_update_keyboard, op_gamepad_rumble],
esm = [
    dir "src/input",
    "00_input_clock.js",
    "01_touch.js",
    "02_keyboard.js",
    "03_mouse.js",
    "04_gamepad.js",
    "05_composition.js",
]
);

pub fn touch_extensions() -> Vec<Extension> {
    vec![host_v8_touch::init()]
}

pub fn touch_lazy_extensions() -> Vec<Extension> {
    vec![host_v8_touch::lazy_init()]
}
