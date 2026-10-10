//! The gamepad sample the Android SDK writes into a direct buffer, read back.
//!
//! Its own module, beside the JNI entry that calls it, so the layout is tested
//! on every host rather than only on a device.

use shared::protocol::host_cmd::{
    GAMEPAD_MAX_AXES, GAMEPAD_MAX_BUTTONS, GamepadButtonState, GamepadState,
};

/// Bytes a sample of `axis_count` axes and `button_count` buttons takes.
pub(crate) const fn gamepad_state_bytes(axis_count: usize, button_count: usize) -> usize {
    axis_count * 4 + button_count * 8
}

/// The sample `onGamepadState` reads: `axis_count` f32 axes, then per button a
/// u32 flags word (bit 0 pressed, bit 1 touched) and an f32 value, native order.
/// The caller has checked the counts are within the maxima and that `bytes`
/// holds them.
pub(crate) fn gamepad_state(
    index: u32,
    axis_count: usize,
    button_count: usize,
    bytes: &[u8],
    timestamp_ms: f64,
) -> GamepadState {
    let word =
        |at: usize| u32::from_ne_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let mut axes = [0.0f32; GAMEPAD_MAX_AXES];
    for (i, axis) in axes.iter_mut().take(axis_count).enumerate() {
        *axis = f32::from_bits(word(i * 4));
    }
    let mut buttons = [GamepadButtonState::default(); GAMEPAD_MAX_BUTTONS];
    for (i, button) in buttons.iter_mut().take(button_count).enumerate() {
        let at = axis_count * 4 + i * 8;
        let flags = word(at);
        *button = GamepadButtonState {
            pressed: flags & 1 != 0,
            touched: flags & 2 != 0,
            value: f32::from_bits(word(at + 4)),
        };
    }
    GamepadState {
        index,
        axis_count: axis_count as u8,
        button_count: button_count as u8,
        axes,
        buttons,
        timestamp_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axes_then_a_flags_word_and_value_per_button() {
        let mut bytes = Vec::new();
        for axis in [0.5f32, -1.0] {
            bytes.extend_from_slice(&axis.to_ne_bytes());
        }
        for (flags, value) in [(1u32 | 2, 1.0f32), (2, 0.25), (0, 0.0)] {
            bytes.extend_from_slice(&flags.to_ne_bytes());
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        assert_eq!(bytes.len(), gamepad_state_bytes(2, 3));

        let state = gamepad_state(3, 2, 3, &bytes, 12.5);
        assert_eq!(
            (state.index, state.axis_count, state.button_count),
            (3, 2, 3)
        );
        assert_eq!(&state.axes[..3], &[0.5, -1.0, 0.0]);
        assert_eq!(
            &state.buttons[..4],
            &[
                GamepadButtonState {
                    pressed: true,
                    touched: true,
                    value: 1.0
                },
                GamepadButtonState {
                    pressed: false,
                    touched: true,
                    value: 0.25
                },
                GamepadButtonState::default(),
                GamepadButtonState::default(),
            ]
        );
        assert_eq!(state.timestamp_ms, 12.5);
    }

    #[test]
    fn the_standard_pad_fits_the_maxima() {
        let bytes = vec![0u8; gamepad_state_bytes(GAMEPAD_MAX_AXES, GAMEPAD_MAX_BUTTONS)];
        let state = gamepad_state(0, GAMEPAD_MAX_AXES, GAMEPAD_MAX_BUTTONS, &bytes, 0.0);
        assert_eq!(state.button_count as usize, GAMEPAD_MAX_BUTTONS);
    }
}
