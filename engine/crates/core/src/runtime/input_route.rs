//! Where a host's input goes, whichever execution runs the content.
//!
//! A host sends touches, keys, the mouse, the soft keyboard, IME composition
//! and gamepads as `HostCommand`s. Each becomes one call of a function on the
//! engine's host bridge -- `_internalEnqueueRawTouchEvent`,
//! `_internalTriggerKeyDown`, ... -- with the same arguments in both
//! executions: the embedded runtime calls it in this process, the external
//! session sends it to the producer as a service event, which calls the same
//! function of the same engine JavaScript there. [`InputSink`] is that call;
//! [`route`] is the one place that decides which calls a command makes and in
//! what order it updates [`InputState`], so the two executions cannot drift in
//! what a key press or a lost focus means to content.

use shared::protocol::host_cmd::{GamepadState, HostCommand, TouchPoint, TouchType};

use super::input_state::{InputRetraction, InputState};

/// One call of the engine's host bridge per method, with its arguments as the
/// bridge function takes them.
pub(crate) trait InputSink {
    /// `_internalEnqueueRawTouchEvent(type, points, count, timeStamp)`.
    fn touch(&mut self, touch_type: TouchType, points: &[TouchPoint], timestamp_ms: i64);
    /// `_internalTriggerFocusChanged(focused)`.
    fn focus_changed(&mut self, focused: bool);
    /// `_internalTriggerKeyboardInput(value)`.
    fn keyboard_input(&mut self, value: &str);
    /// `_internalTriggerKeyboardHeightChange(height)`.
    fn keyboard_height_change(&mut self, height: f64);
    /// `_internalTriggerKeyboardConfirm(value)`.
    fn keyboard_confirm(&mut self, value: &str);
    /// `_internalTriggerKeyboardComplete(value)`.
    fn keyboard_complete(&mut self, value: &str);
    /// `_internalTriggerCompositionStart(data)`.
    fn composition_start(&mut self, data: &str);
    /// `_internalTriggerCompositionUpdate(data)`.
    fn composition_update(&mut self, data: &str);
    /// `_internalTriggerCompositionEnd(data)`.
    fn composition_end(&mut self, data: &str);
    /// `_internalTriggerGamepadConnected(index, id, mapping, axes, buttons)`.
    fn gamepad_connected(
        &mut self,
        index: u32,
        id: &str,
        mapping: &str,
        axis_count: u8,
        button_count: u8,
    );
    /// `_internalTriggerGamepadDisconnected(index)`.
    fn gamepad_disconnected(&mut self, index: u32);
    /// `_internalTriggerGamepadState(index, timeStamp, packed)`.
    fn gamepad_state(&mut self, state: &GamepadState);
    /// `_internalTriggerKeyDown(key, code, timeStamp, modifiers, repeat)`.
    fn key_down(&mut self, key: &str, code: &str, timestamp_ms: f64, modifiers: u32, repeat: bool);
    /// `_internalTriggerKeyUp(key, code, timeStamp, modifiers, repeat)`.
    fn key_up(&mut self, key: &str, code: &str, timestamp_ms: f64, modifiers: u32, repeat: bool);
    /// `_internalTriggerMouseDown(x, y, button, timeStamp)`.
    fn mouse_down(&mut self, x: f32, y: f32, button: u32, timestamp_ms: f64);
    /// `_internalTriggerMouseMove(x, y, button, timeStamp)`.
    fn mouse_move(&mut self, x: f32, y: f32, button: u32, timestamp_ms: f64);
    /// `_internalTriggerMouseUp(x, y, button, timeStamp)`.
    fn mouse_up(&mut self, x: f32, y: f32, button: u32, timestamp_ms: f64);
    /// `_internalTriggerWheel(dx, dy, dz, timeStamp, deltaMode)`.
    fn wheel(
        &mut self,
        delta_x: f64,
        delta_y: f64,
        delta_z: f64,
        delta_mode: u32,
        timestamp_ms: f64,
    );
}

/// Deliver `command` to `sink` if it is input, and answer with it back if it
/// is not.
///
/// `state` is what the content has been told is held down -- touches, mouse
/// buttons, keys, an open composition -- kept so that losing focus can release
/// each of them the way the platform would have, before `focusChanged(false)`.
/// The order of observing and dispatching is part of the contract: a touch is
/// observed before it is delivered, a key after.
pub(crate) fn route(
    state: &mut InputState,
    sink: &mut impl InputSink,
    command: HostCommand,
) -> Option<HostCommand> {
    match command {
        HostCommand::OnFocusChanged { focused } => {
            if !focused {
                state.retract_for_focus_loss(|retraction| match retraction {
                    InputRetraction::TouchCancel(touch) => {
                        let count = usize::from(touch.count).min(touch.points.len());
                        sink.touch(touch.touch_type, &touch.points[..count], touch.timestamp_ms);
                    }
                    InputRetraction::MouseUp {
                        x,
                        y,
                        button,
                        timestamp_ms,
                    } => sink.mouse_up(x, y, button, timestamp_ms),
                    InputRetraction::KeyUp {
                        key,
                        code,
                        timestamp_ms,
                        modifiers,
                    } => sink.key_up(&key, &code, timestamp_ms, modifiers, false),
                    InputRetraction::CompositionEnd => sink.composition_end(""),
                });
            }
            sink.focus_changed(focused);
        }
        HostCommand::OnTouch(touch) => {
            let count = usize::from(touch.count).min(touch.points.len());
            state.observe_touch(&touch);
            sink.touch(touch.touch_type, &touch.points[..count], touch.timestamp_ms);
        }
        HostCommand::OnKeyboardInput { value, .. } => sink.keyboard_input(&value),
        HostCommand::OnKeyboardHeightChange { height, .. } => sink.keyboard_height_change(height),
        HostCommand::OnKeyboardConfirm { value, .. } => sink.keyboard_confirm(&value),
        HostCommand::OnKeyboardComplete { value, .. } => sink.keyboard_complete(&value),
        HostCommand::OnCompositionStart { data } => {
            sink.composition_start(&data);
            state.observe_composition_start();
        }
        HostCommand::OnCompositionUpdate { data } => sink.composition_update(&data),
        HostCommand::OnCompositionEnd { data } => {
            sink.composition_end(&data);
            state.observe_composition_end();
        }
        HostCommand::OnGamepadConnected {
            index,
            id,
            mapping,
            axis_count,
            button_count,
        } => sink.gamepad_connected(index, &id, &mapping, axis_count, button_count),
        HostCommand::OnGamepadDisconnected { index } => sink.gamepad_disconnected(index),
        HostCommand::OnGamepadState(state_sample) => sink.gamepad_state(&state_sample),
        HostCommand::OnKeyDown {
            key,
            code,
            timestamp_ms,
            modifiers,
            repeat,
        } => {
            sink.key_down(&key, &code, timestamp_ms, modifiers, repeat);
            state.observe_key_down(key, code, timestamp_ms, modifiers);
        }
        HostCommand::OnKeyUp {
            key,
            code,
            timestamp_ms,
            modifiers,
            repeat,
        } => {
            sink.key_up(&key, &code, timestamp_ms, modifiers, repeat);
            state.observe_key_up(&code);
        }
        HostCommand::OnMouseDown {
            x,
            y,
            button,
            timestamp_ms,
        } => {
            state.observe_mouse_down(x, y, button, timestamp_ms);
            sink.mouse_down(x, y, button, timestamp_ms);
        }
        HostCommand::OnMouseMove {
            x,
            y,
            button,
            timestamp_ms,
        } => {
            state.observe_mouse_move(x, y, button, timestamp_ms);
            sink.mouse_move(x, y, button, timestamp_ms);
        }
        HostCommand::OnMouseUp {
            x,
            y,
            button,
            timestamp_ms,
        } => {
            state.observe_mouse_up(x, y, button, timestamp_ms);
            sink.mouse_up(x, y, button, timestamp_ms);
        }
        HostCommand::OnWheel {
            delta_x,
            delta_y,
            delta_z,
            delta_mode,
            timestamp_ms,
        } => sink.wheel(delta_x, delta_y, delta_z, delta_mode, timestamp_ms),
        other => return Some(other),
    }
    None
}
