//! Where a host's input goes, whichever execution runs the content.
//!
//! A host sends touches, keys, the mouse, the soft keyboard, IME composition
//! and gamepads as `HostCommand`s, and what the device reports -- its motion
//! sensors, its Bluetooth -- the same way. Each becomes one call of a function on the
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
    /// `_internalTriggerGamepadConnected(index, id, mapping, axes, buttons, haptics)`.
    fn gamepad_connected(
        &mut self,
        index: u32,
        id: &str,
        mapping: &str,
        axis_count: u8,
        button_count: u8,
        haptics: u8,
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
    /// `_internalTriggerDeviceMotionChange(alpha, beta, gamma)`.
    fn device_motion(&mut self, alpha: f64, beta: f64, gamma: f64);
    /// `_internalTriggerGyroscopeChange(x, y, z)`.
    fn gyroscope(&mut self, x: f64, y: f64, z: f64);
    /// `_internalTriggerAccelerometerChange(x, y, z)`.
    fn accelerometer(&mut self, x: f64, y: f64, z: f64);
    /// `_internalTriggerCompassChange(direction, accuracy)`.
    fn compass(&mut self, direction: f64, accuracy: &str);
    /// `_internalTriggerDeviceOrientationChange(value)`.
    fn device_orientation(&mut self, value: &str);
    /// `_internalTriggerBluetoothAdapterStateChange(available, discovering)`.
    fn bluetooth_adapter_state_change(&mut self, available: bool, discovering: bool);
    /// `_internalTriggerBluetoothDeviceFound(devicesJson)`.
    fn bluetooth_device_found(&mut self, devices_json: &str);
    /// `_internalTriggerBeaconUpdate(beaconsJson)`.
    fn beacon_update(&mut self, beacons_json: &str);
    /// `_internalTriggerBeaconServiceChange(available, discovering)`.
    fn beacon_service_change(&mut self, available: bool, discovering: bool);
    /// `_internalTriggerBLEConnectionStateChange(deviceId, connected)`.
    fn ble_connection_state_change(&mut self, device_id: &str, connected: bool);
    /// `_internalTriggerBLECharacteristicValueChange(deviceId, serviceId,
    /// characteristicId, value)`, the value as its ArrayBuffer.
    fn ble_characteristic_value_change(
        &mut self,
        device_id: &str,
        service_id: &str,
        characteristic_id: &str,
        value: &[u8],
    );
    /// `_internalTriggerBLEMTUChange(deviceId, mtu)`.
    fn ble_mtu_change(&mut self, device_id: &str, mtu: u32);
    /// `_internalOnRecorderFrameData(recordingId, frameBuffer, isLastFrame)`,
    /// the frame as its ArrayBuffer.
    fn recorder_frame(&mut self, recording_id: u32, data: Vec<u8>, is_last_frame: bool);
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
            haptics,
        } => sink.gamepad_connected(index, &id, &mapping, axis_count, button_count, haptics),
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
        // The generation these carry was read before routing, by the caller's
        // retired-callback check: nothing here may decide on it again.
        HostCommand::OnDeviceMotionChange {
            alpha, beta, gamma, ..
        } => sink.device_motion(alpha, beta, gamma),
        HostCommand::OnGyroscopeChange { x, y, z, .. } => sink.gyroscope(x, y, z),
        HostCommand::OnAccelerometerChange { x, y, z, .. } => sink.accelerometer(x, y, z),
        HostCommand::OnCompassChange {
            direction,
            accuracy,
            ..
        } => sink.compass(direction, &accuracy),
        HostCommand::OnDeviceOrientationChange { value } => sink.device_orientation(&value),
        HostCommand::OnBluetoothAdapterStateChange {
            available,
            discovering,
        } => sink.bluetooth_adapter_state_change(available, discovering),
        HostCommand::OnBluetoothDeviceFound { devices_json } => {
            sink.bluetooth_device_found(&devices_json)
        }
        HostCommand::OnBeaconUpdate { beacons_json } => sink.beacon_update(&beacons_json),
        HostCommand::OnBeaconServiceChange {
            available,
            discovering,
        } => sink.beacon_service_change(available, discovering),
        HostCommand::OnBLEConnectionStateChange {
            device_id,
            connected,
        } => sink.ble_connection_state_change(&device_id, connected),
        HostCommand::OnBLECharacteristicValueChange(ble) => sink.ble_characteristic_value_change(
            ble.device_id(),
            ble.service_id(),
            ble.characteristic_id(),
            ble.value(),
        ),
        HostCommand::OnBLEMTUChange { device_id, mtu } => sink.ble_mtu_change(&device_id, mtu),
        // The frame's byte reservation is released once the frame is handed
        // on, whichever execution takes it.
        HostCommand::RecorderFrameData {
            recording_id,
            data,
            is_last_frame,
            credit: _credit,
        } => sink.recorder_frame(recording_id, data, is_last_frame),
        other => return Some(other),
    }
    None
}
