//! Host events on the external session: input the host received, sent to the
//! producer as service EVENT records.
//!
//! [`ServiceEventSink`] is the external execution's [`InputSink`]: where the
//! embedded runtime calls a host-bridge function in process, this writes one
//! EVENT naming it (`contracts/runtime/host-events.json`) with the call's
//! arguments as tagged values in the function's parameter order, and the
//! producer makes the same call on the same engine JavaScript. Each argument is
//! the value the embedded bindings hand V8 (`runtime-v8/src/js_bindings.rs`):
//! numbers as numbers, text as strings, a touch's points as their 20-byte
//! records, which the producer passes as an ArrayBuffer.

use frame_wire::value::OwnedValue;
use shared::protocol::host_cmd::{GamepadState, TouchPoint, TouchType};

use super::external_services::ServiceOutbox;
use super::input_route::InputSink;

macro_rules! host_events {
    ($($name:ident = $id:literal,)*) => {
        /// One constant per event, named as the host-bridge function is.
        #[allow(non_upper_case_globals)]
        pub(crate) mod event {
            $(pub(crate) const $name: u32 = $id;)*
        }

        /// Every event and its number, for the contract test.
        #[cfg(test)]
        pub(crate) const ALL: &[(&str, u32)] = &[$((stringify!($name), $id),)*];
    };
}

host_events! {
    _internalEnqueueRawTouchEvent = 1,
    _internalTriggerFocusChanged = 2,
    _internalTriggerKeyboardInput = 3,
    _internalTriggerKeyboardHeightChange = 4,
    _internalTriggerKeyboardConfirm = 5,
    _internalTriggerKeyboardComplete = 6,
    _internalTriggerCompositionStart = 7,
    _internalTriggerCompositionUpdate = 8,
    _internalTriggerCompositionEnd = 9,
    _internalTriggerGamepadConnected = 10,
    _internalTriggerGamepadDisconnected = 11,
    _internalTriggerGamepadState = 12,
    _internalTriggerKeyDown = 13,
    _internalTriggerKeyUp = 14,
    _internalTriggerMouseDown = 15,
    _internalTriggerMouseMove = 16,
    _internalTriggerMouseUp = 17,
    _internalTriggerWheel = 18,
}

/// The size of one `MigoTouchPoint` record, as `01_touch.js` reads it.
const TOUCH_POINT_BYTES: usize = 20;

/// Input, as service events on `outbox`.
pub(crate) struct ServiceEventSink<'a> {
    pub(crate) outbox: &'a ServiceOutbox,
}

impl ServiceEventSink<'_> {
    #[inline]
    fn send(&self, event: u32, values: Vec<OwnedValue>) {
        self.outbox.event(event, values);
    }

    fn text(&self, event: u32, text: &str) {
        self.send(event, vec![OwnedValue::Str(text.to_owned())]);
    }

    fn mouse(&self, event: u32, x: f32, y: f32, button: u32, timestamp_ms: f64) {
        self.send(
            event,
            vec![
                OwnedValue::F64(f64::from(x)),
                OwnedValue::F64(f64::from(y)),
                OwnedValue::U32(button),
                OwnedValue::F64(timestamp_ms),
            ],
        );
    }

    fn key(
        &self,
        event: u32,
        key: &str,
        code: &str,
        timestamp_ms: f64,
        modifiers: u32,
        repeat: bool,
    ) {
        self.send(
            event,
            vec![
                OwnedValue::Str(key.to_owned()),
                OwnedValue::Str(code.to_owned()),
                OwnedValue::F64(timestamp_ms),
                OwnedValue::U32(modifiers),
                OwnedValue::Bool(repeat),
            ],
        );
    }
}

/// A touch's points as the little-endian records the engine reads: id, x, y,
/// pressure, flags.
fn touch_records(points: &[TouchPoint]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(points.len() * TOUCH_POINT_BYTES);
    for point in points {
        bytes.extend_from_slice(&point.id.to_le_bytes());
        bytes.extend_from_slice(&point.x.to_le_bytes());
        bytes.extend_from_slice(&point.y.to_le_bytes());
        bytes.extend_from_slice(&point.pressure.to_le_bytes());
        bytes.extend_from_slice(&point.flags.to_le_bytes());
    }
    bytes
}

/// A gamepad sample as the embedded binding packs it: axis count, button
/// count, the axes, then pressed, touched and value per button.
fn gamepad_packed(state: &GamepadState) -> Vec<OwnedValue> {
    let axes = usize::from(state.axis_count).min(state.axes.len());
    let buttons = usize::from(state.button_count).min(state.buttons.len());
    let mut packed = Vec::with_capacity(2 + axes + buttons * 3);
    packed.push(OwnedValue::F64(axes as f64));
    packed.push(OwnedValue::F64(buttons as f64));
    for axis in &state.axes[..axes] {
        packed.push(OwnedValue::F64(f64::from(*axis)));
    }
    for button in &state.buttons[..buttons] {
        packed.push(OwnedValue::F64(if button.pressed { 1.0 } else { 0.0 }));
        packed.push(OwnedValue::F64(if button.touched { 1.0 } else { 0.0 }));
        packed.push(OwnedValue::F64(f64::from(button.value)));
    }
    packed
}

impl InputSink for ServiceEventSink<'_> {
    fn touch(&mut self, touch_type: TouchType, points: &[TouchPoint], timestamp_ms: i64) {
        self.send(
            event::_internalEnqueueRawTouchEvent,
            vec![
                OwnedValue::U32(touch_type as u32),
                OwnedValue::Bytes(touch_records(points)),
                OwnedValue::U32(points.len() as u32),
                OwnedValue::F64(timestamp_ms as f64),
            ],
        );
    }
    fn focus_changed(&mut self, focused: bool) {
        self.send(
            event::_internalTriggerFocusChanged,
            vec![OwnedValue::Bool(focused)],
        );
    }
    fn keyboard_input(&mut self, value: &str) {
        self.text(event::_internalTriggerKeyboardInput, value);
    }
    fn keyboard_height_change(&mut self, height: f64) {
        self.send(
            event::_internalTriggerKeyboardHeightChange,
            vec![OwnedValue::F64(height)],
        );
    }
    fn keyboard_confirm(&mut self, value: &str) {
        self.text(event::_internalTriggerKeyboardConfirm, value);
    }
    fn keyboard_complete(&mut self, value: &str) {
        self.text(event::_internalTriggerKeyboardComplete, value);
    }
    fn composition_start(&mut self, data: &str) {
        self.text(event::_internalTriggerCompositionStart, data);
    }
    fn composition_update(&mut self, data: &str) {
        self.text(event::_internalTriggerCompositionUpdate, data);
    }
    fn composition_end(&mut self, data: &str) {
        self.text(event::_internalTriggerCompositionEnd, data);
    }
    fn gamepad_connected(
        &mut self,
        index: u32,
        id: &str,
        mapping: &str,
        axis_count: u8,
        button_count: u8,
    ) {
        self.send(
            event::_internalTriggerGamepadConnected,
            vec![
                OwnedValue::U32(index),
                OwnedValue::Str(id.to_owned()),
                OwnedValue::Str(mapping.to_owned()),
                OwnedValue::U32(u32::from(axis_count)),
                OwnedValue::U32(u32::from(button_count)),
            ],
        );
    }
    fn gamepad_disconnected(&mut self, index: u32) {
        self.send(
            event::_internalTriggerGamepadDisconnected,
            vec![OwnedValue::U32(index)],
        );
    }
    fn gamepad_state(&mut self, state: &GamepadState) {
        self.send(
            event::_internalTriggerGamepadState,
            vec![
                OwnedValue::U32(state.index),
                OwnedValue::F64(state.timestamp_ms),
                OwnedValue::Array(gamepad_packed(state)),
            ],
        );
    }
    fn key_down(&mut self, key: &str, code: &str, timestamp_ms: f64, modifiers: u32, repeat: bool) {
        self.key(
            event::_internalTriggerKeyDown,
            key,
            code,
            timestamp_ms,
            modifiers,
            repeat,
        );
    }
    fn key_up(&mut self, key: &str, code: &str, timestamp_ms: f64, modifiers: u32, repeat: bool) {
        self.key(
            event::_internalTriggerKeyUp,
            key,
            code,
            timestamp_ms,
            modifiers,
            repeat,
        );
    }
    fn mouse_down(&mut self, x: f32, y: f32, button: u32, timestamp_ms: f64) {
        self.mouse(event::_internalTriggerMouseDown, x, y, button, timestamp_ms);
    }
    fn mouse_move(&mut self, x: f32, y: f32, button: u32, timestamp_ms: f64) {
        self.mouse(event::_internalTriggerMouseMove, x, y, button, timestamp_ms);
    }
    fn mouse_up(&mut self, x: f32, y: f32, button: u32, timestamp_ms: f64) {
        self.mouse(event::_internalTriggerMouseUp, x, y, button, timestamp_ms);
    }
    fn wheel(
        &mut self,
        delta_x: f64,
        delta_y: f64,
        delta_z: f64,
        delta_mode: u32,
        timestamp_ms: f64,
    ) {
        // The embedded binding's order: the deltas, the timestamp, then the
        // mode, appended last so a stale four-parameter function still binds.
        self.send(
            event::_internalTriggerWheel,
            vec![
                OwnedValue::F64(delta_x),
                OwnedValue::F64(delta_y),
                OwnedValue::F64(delta_z),
                OwnedValue::F64(timestamp_ms),
                OwnedValue::U32(delta_mode),
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host's table and the contract, entry for entry.
    #[test]
    fn the_host_table_is_the_contract() {
        let contract: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../contracts/runtime/host-events.json"
        ))
        .expect("the contract is JSON");
        let events = contract["events"].as_object().expect("an events object");
        let mut declared: Vec<(String, u32)> = events
            .iter()
            .map(|(name, id)| (name.clone(), id.as_u64().expect("a number") as u32))
            .collect();
        declared.sort_by_key(|(_, id)| *id);
        let mut here: Vec<(String, u32)> = ALL
            .iter()
            .map(|(name, id)| (name.to_string(), *id))
            .collect();
        here.sort_by_key(|(_, id)| *id);
        assert_eq!(here, declared);
    }

    /// The points are the 20-byte records `01_touch.js` reads, in order.
    #[test]
    fn a_touch_is_its_points_as_the_engine_reads_them() {
        let point = TouchPoint {
            id: 7,
            x: 1.5,
            y: -2.0,
            pressure: 0.25,
            flags: 3,
        };
        let bytes = touch_records(&[point]);
        assert_eq!(bytes.len(), TOUCH_POINT_BYTES);
        assert_eq!(&bytes[0..4], &7u32.to_le_bytes());
        assert_eq!(&bytes[4..8], &1.5f32.to_le_bytes());
        assert_eq!(&bytes[8..12], &(-2.0f32).to_le_bytes());
        assert_eq!(&bytes[12..16], &0.25f32.to_le_bytes());
        assert_eq!(&bytes[16..20], &3u32.to_le_bytes());
    }
}
