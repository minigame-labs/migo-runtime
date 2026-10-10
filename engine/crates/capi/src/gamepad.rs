//! Gamepads, inbound.
//!
//! Mainstream mini-game platforms have no gamepad API, so the shape here is the Web one Migo replaces: a
//! host announces a pad, pushes samples while it is connected, and withdraws
//! it. Content polls `getGamepads()` -- the Web API is polled, not evented --
//! so a sample updates stored state rather than being delivered to a listener.

use std::sync::atomic::{AtomicU32, Ordering};

use shared::protocol::host_cmd::{
    GAMEPAD_MAX_AXES, GAMEPAD_MAX_BUTTONS, GamepadButtonState, GamepadState, HostCommand,
};

use crate::panic_barrier::guard;
use crate::{MigoSession, map_ingress_result, pin_session};
use migo_capi_abi::{
    MIGO_ERROR_INVALID_ARGUMENT, MIGO_ERROR_INVALID_STATE, MIGO_ERROR_WOULD_BLOCK, MigoResult,
};

pub(crate) use migo_capi_abi::input::MIGO_GAMEPAD_MAX_COUNT;
use migo_capi_abi::input::{
    MIGO_GAMEPAD_MAX_AXES, MIGO_GAMEPAD_MAX_BUTTONS, ValidatedGamepadConnection,
    ValidatedGamepadState,
};
pub use migo_capi_abi::input::{MigoGamepadButton, MigoGamepadInfo, MigoGamepadStateEvent};

#[cfg(test)]
use migo_capi_abi::input::{MIGO_GAMEPAD_BUTTON_FLAG_PRESSED, MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED};

const _: () = assert!(GAMEPAD_MAX_AXES == MIGO_GAMEPAD_MAX_AXES);
const _: () = assert!(shared::protocol::host_cmd::GAMEPAD_MAX_COUNT == MIGO_GAMEPAD_MAX_COUNT);
const _: () = assert!(GAMEPAD_MAX_BUTTONS == MIGO_GAMEPAD_MAX_BUTTONS);

// Four bits cover 0..=8 axes and five cover 0..=20 buttons. The upper bits
// carry a transition latch and an in-flight sample reader count so connect,
// sample and disconnect stay ordered without a Session mutex on the hot path.
const AXIS_MASK: u32 = 0x0f;
const BUTTON_SHIFT: u32 = 4;
const BUTTON_MASK: u32 = 0x1f << BUTTON_SHIFT;
const CONNECTED: u32 = 1 << 9;
const TRANSITION: u32 = 1 << 10;
const READER_SHIFT: u32 = 11;
const READER_ONE: u32 = 1 << READER_SHIFT;
const READER_MASK: u32 = u32::MAX << READER_SHIFT;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TopologyError {
    Busy,
    AlreadyConnected,
    Disconnected,
    Mismatch,
}

pub(crate) struct GamepadTopology {
    slots: [AtomicU32; MIGO_GAMEPAD_MAX_COUNT],
}

impl GamepadTopology {
    pub(crate) fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }

    fn begin_connect(
        &self,
        index: u32,
        axis_count: u8,
        button_count: u8,
    ) -> Result<TopologyChange<'_>, TopologyError> {
        let slot = &self.slots[index as usize];
        let topology = encode_topology(axis_count, button_count);
        slot.compare_exchange(
            0,
            TRANSITION | topology,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map_err(|observed| {
            if observed & TRANSITION != 0 {
                TopologyError::Busy
            } else {
                TopologyError::AlreadyConnected
            }
        })?;
        Ok(TopologyChange {
            slot,
            rollback: 0,
            commit: CONNECTED | topology,
            committed: false,
        })
    }

    fn begin_disconnect(&self, index: u32) -> Result<TopologyChange<'_>, TopologyError> {
        let slot = &self.slots[index as usize];
        loop {
            let observed = slot.load(Ordering::Acquire);
            if observed & TRANSITION != 0 || observed & READER_MASK != 0 {
                return Err(TopologyError::Busy);
            }
            if observed & CONNECTED == 0 {
                return Err(TopologyError::Disconnected);
            }
            match slot.compare_exchange(
                observed,
                observed | TRANSITION,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(TopologyChange {
                        slot,
                        rollback: observed,
                        commit: 0,
                        committed: false,
                    });
                }
                Err(_) => continue,
            }
        }
    }

    fn begin_sample(
        &self,
        index: u32,
        axis_count: u8,
        button_count: u8,
    ) -> Result<GamepadSample<'_>, TopologyError> {
        let slot = &self.slots[index as usize];
        let expected_topology = encode_topology(axis_count, button_count);
        loop {
            let observed = slot.load(Ordering::Acquire);
            if observed & TRANSITION != 0 {
                return Err(TopologyError::Busy);
            }
            if observed & CONNECTED == 0 {
                return Err(TopologyError::Disconnected);
            }
            if observed & (AXIS_MASK | BUTTON_MASK) != expected_topology {
                return Err(TopologyError::Mismatch);
            }
            if observed & READER_MASK == READER_MASK {
                return Err(TopologyError::Busy);
            }
            match slot.compare_exchange_weak(
                observed,
                observed + READER_ONE,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(GamepadSample { slot }),
                Err(_) => continue,
            }
        }
    }
}

fn encode_topology(axis_count: u8, button_count: u8) -> u32 {
    debug_assert!(axis_count as usize <= MIGO_GAMEPAD_MAX_AXES);
    debug_assert!(button_count as usize <= MIGO_GAMEPAD_MAX_BUTTONS);
    u32::from(axis_count) | (u32::from(button_count) << BUTTON_SHIFT)
}

struct TopologyChange<'a> {
    slot: &'a AtomicU32,
    rollback: u32,
    commit: u32,
    committed: bool,
}

impl TopologyChange<'_> {
    fn commit(mut self) {
        self.slot.store(self.commit, Ordering::Release);
        self.committed = true;
    }
}

impl Drop for TopologyChange<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.slot.store(self.rollback, Ordering::Release);
        }
    }
}

struct GamepadSample<'a> {
    slot: &'a AtomicU32,
}

impl Drop for GamepadSample<'_> {
    fn drop(&mut self) {
        let previous = self.slot.fetch_sub(READER_ONE, Ordering::Release);
        debug_assert!(previous & READER_MASK != 0);
    }
}

fn map_topology_error(error: TopologyError) -> MigoResult {
    match error {
        TopologyError::Busy => MIGO_ERROR_WOULD_BLOCK,
        TopologyError::AlreadyConnected | TopologyError::Disconnected => MIGO_ERROR_INVALID_STATE,
        TopologyError::Mismatch => MIGO_ERROR_INVALID_ARGUMENT,
    }
}

/// Translate a validated sample into the engine's state payload.
///
fn validated_to_gamepad_state(event: ValidatedGamepadState) -> GamepadState {
    let (index, axis_count, button_count, axes, source_buttons, timestamp_ms) = event.into_parts();
    let mut buttons = [GamepadButtonState::default(); GAMEPAD_MAX_BUTTONS];
    for (destination, source) in buttons.iter_mut().zip(source_buttons) {
        *destination = GamepadButtonState {
            pressed: source.pressed(),
            touched: source.touched(),
            value: source.value(),
        };
    }

    GamepadState {
        index,
        axis_count,
        button_count,
        axes,
        buttons,
        timestamp_ms,
    }
}

/// # Safety
/// `axes` and `buttons` must hold at least their announced counts.
#[cfg(test)]
unsafe fn to_gamepad_state(event: &MigoGamepadStateEvent) -> Result<GamepadState, MigoResult> {
    unsafe { event.validate() }.map(validated_to_gamepad_state)
}

/// Announce a gamepad, or withdraw one by passing `connected` as 0.
///
/// # Safety
/// `session` must be live; `info` must point at a `MigoGamepadInfo` whose
/// strings are NUL-terminated. Both are borrowed for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_set_gamepad_connected(
    session: *mut MigoSession,
    info: *const MigoGamepadInfo,
    connected: u32,
) -> MigoResult {
    guard("migo_session_set_gamepad_connected", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let connection = match unsafe { MigoGamepadInfo::parse_connection(info, connected) } {
            Ok(connection) => connection,
            Err(error) => return error,
        };
        let ingress = match session.active_ingress() {
            Ok(ingress) => ingress,
            Err(error) => return error,
        };
        let (command, reservation) = match connection {
            ValidatedGamepadConnection::Disconnected { index } => {
                let reservation = match session.gamepad_topology.begin_disconnect(index) {
                    Ok(reservation) => reservation,
                    Err(error) => return map_topology_error(error),
                };
                (HostCommand::OnGamepadDisconnected { index }, reservation)
            }
            ValidatedGamepadConnection::Connected {
                index,
                id,
                mapping,
                axis_count,
                button_count,
            } => {
                let reservation =
                    match session
                        .gamepad_topology
                        .begin_connect(index, axis_count, button_count)
                    {
                        Ok(reservation) => reservation,
                        Err(error) => return map_topology_error(error),
                    };
                (
                    HostCommand::OnGamepadConnected {
                        index,
                        id,
                        mapping,
                        axis_count,
                        button_count,
                    },
                    reservation,
                )
            }
        };
        let result = ingress.try_send_gamepad_connection(command);
        if result.is_ok() {
            reservation.commit();
        }
        map_ingress_result(&session, "migo_session_set_gamepad_connected", result)
    })
}

/// Push one sample of a connected gamepad's axes and buttons.
///
/// # Safety
/// `session` must be live; `event`'s `axes` and `buttons` must each hold at
/// least their announced counts. All are borrowed for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_send_gamepad_state(
    session: *mut MigoSession,
    event: *const MigoGamepadStateEvent,
) -> MigoResult {
    guard("migo_session_send_gamepad_state", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let state = match unsafe { MigoGamepadStateEvent::parse(event) } {
            Ok(event) => validated_to_gamepad_state(event),
            Err(error) => return error,
        };
        let ingress = match session.active_ingress() {
            Ok(ingress) => ingress,
            Err(error) => return error,
        };
        let _sample = match session.gamepad_topology.begin_sample(
            state.index,
            state.axis_count,
            state.button_count,
        ) {
            Ok(sample) => sample,
            Err(error) => return map_topology_error(error),
        };
        // A dropped sample is harmless: the API is polled, so the next one
        // replaces it wholesale. Reported anyway, because a host seeing this
        // every frame is sampling faster than the engine drains.
        map_ingress_result(
            &session,
            "migo_session_send_gamepad_state",
            ingress.try_send_gamepad_state(state),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_capi_abi::{MIGO_ABI_VERSION_CURRENT, VersionedHeader};
    use std::mem::size_of;

    #[test]
    fn topology_is_published_only_after_connect_commit_and_rolls_back_on_failure() {
        let topology = GamepadTopology::new();
        assert!(matches!(
            topology.begin_sample(0, 2, 4),
            Err(TopologyError::Disconnected)
        ));

        let pending = topology
            .begin_connect(0, 2, 4)
            .unwrap_or_else(|error| panic!("connect reservation failed: {error:?}"));
        assert!(matches!(
            topology.begin_sample(0, 2, 4),
            Err(TopologyError::Busy)
        ));
        drop(pending); // models a rejected Host enqueue
        assert!(matches!(
            topology.begin_sample(0, 2, 4),
            Err(TopologyError::Disconnected)
        ));

        topology
            .begin_connect(0, 2, 4)
            .unwrap_or_else(|error| panic!("connect retry failed: {error:?}"))
            .commit();
        assert!(topology.begin_sample(0, 2, 4).is_ok());
        assert!(matches!(
            topology.begin_connect(0, 2, 4),
            Err(TopologyError::AlreadyConnected)
        ));
    }

    #[test]
    fn samples_must_match_topology_and_disconnect_cannot_overtake_one() {
        let topology = GamepadTopology::new();
        topology
            .begin_connect(3, 4, 17)
            .unwrap_or_else(|error| panic!("connect reservation failed: {error:?}"))
            .commit();

        assert!(matches!(
            topology.begin_sample(3, 3, 17),
            Err(TopologyError::Mismatch)
        ));
        assert!(matches!(
            topology.begin_sample(3, 4, 16),
            Err(TopologyError::Mismatch)
        ));

        let sample = topology
            .begin_sample(3, 4, 17)
            .unwrap_or_else(|error| panic!("matching sample failed: {error:?}"));
        assert!(matches!(
            topology.begin_disconnect(3),
            Err(TopologyError::Busy)
        ));
        drop(sample);

        let pending = topology
            .begin_disconnect(3)
            .unwrap_or_else(|error| panic!("disconnect reservation failed: {error:?}"));
        assert!(matches!(
            topology.begin_sample(3, 4, 17),
            Err(TopologyError::Busy)
        ));
        drop(pending); // rejected enqueue restores the connected topology
        assert!(topology.begin_sample(3, 4, 17).is_ok());

        topology
            .begin_disconnect(3)
            .unwrap_or_else(|error| panic!("disconnect retry failed: {error:?}"))
            .commit();
        assert!(matches!(
            topology.begin_sample(3, 4, 17),
            Err(TopologyError::Disconnected)
        ));
        assert!(matches!(
            topology.begin_disconnect(3),
            Err(TopologyError::Disconnected)
        ));
    }

    fn button(pressed: bool, touched: bool, value: f32) -> MigoGamepadButton {
        let mut flags = 0;
        if pressed {
            flags |= MIGO_GAMEPAD_BUTTON_FLAG_PRESSED;
        }
        if touched {
            flags |= MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED;
        }
        MigoGamepadButton { flags, value }
    }

    fn state_event(axes: &[f32], buttons: &[MigoGamepadButton]) -> MigoGamepadStateEvent {
        MigoGamepadStateEvent {
            header: VersionedHeader {
                struct_size: size_of::<MigoGamepadStateEvent>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            index: 0,
            axis_count: axes.len() as u32,
            button_count: buttons.len() as u32,
            reserved0: 0,
            axes: axes.as_ptr(),
            buttons: buttons.as_ptr(),
            timestamp_ms: 16.0,
        }
    }

    /// Every axis and every button flag must land where content reads it. A
    /// swapped flag bit turns a resting trigger into a held one, which content
    /// sees as a button nobody pressed.
    #[test]
    fn a_sample_arrives_with_every_axis_and_button_in_place() {
        let axes = [-1.0f32, 0.0, 0.5, 1.0];
        let buttons = [
            button(true, true, 1.0),
            button(false, true, 0.25),
            button(false, false, 0.0),
        ];
        let state =
            unsafe { to_gamepad_state(&state_event(&axes, &buttons)) }.expect("well-formed");

        assert_eq!(state.axis_count, 4);
        assert_eq!(state.button_count, 3);
        for (index, expected) in axes.iter().enumerate() {
            assert_eq!(state.axes[index], *expected, "axis {index}");
        }
        assert!(state.buttons[0].pressed && state.buttons[0].touched);
        assert_eq!(state.buttons[0].value, 1.0);
        assert!(!state.buttons[1].pressed && state.buttons[1].touched);
        assert_eq!(state.buttons[1].value, 0.25);
        assert!(!state.buttons[2].pressed && !state.buttons[2].touched);
    }

    /// `pressed` is carried, not derived: a device picks its own threshold, and
    /// a trigger held at 0.25 may or may not count as pressed depending on the
    /// pad. Deriving it here would overrule the device.
    #[test]
    fn pressed_is_independent_of_value() {
        let buttons = [button(true, false, 0.0), button(false, false, 0.9)];
        let state = unsafe { to_gamepad_state(&state_event(&[], &buttons)) }.expect("well-formed");
        assert!(state.buttons[0].pressed, "a pressed button at rest value");
        assert!(
            !state.buttons[1].pressed,
            "an unpressed button near full travel"
        );
    }

    /// Slots past the announced counts belong to nobody.
    #[test]
    fn entries_beyond_the_counts_keep_their_defaults() {
        let axes = [1.0f32];
        let buttons = [button(true, true, 1.0)];
        let state =
            unsafe { to_gamepad_state(&state_event(&axes, &buttons)) }.expect("well-formed");
        assert_eq!(state.axes[1], 0.0);
        assert_eq!(state.buttons[1], GamepadButtonState::default());
    }

    /// Truncating would silently drop a button content is watching, so a sample
    /// bigger than the runtime carries is refused rather than shortened.
    #[test]
    fn an_oversized_sample_is_rejected_rather_than_truncated() {
        let axes = [0.0f32; GAMEPAD_MAX_AXES + 1];
        assert_eq!(
            unsafe { to_gamepad_state(&state_event(&axes, &[])) }.err(),
            Some(MIGO_ERROR_INVALID_ARGUMENT)
        );
        let buttons = [button(false, false, 0.0); GAMEPAD_MAX_BUTTONS + 1];
        assert_eq!(
            unsafe { to_gamepad_state(&state_event(&[], &buttons)) }.err(),
            Some(MIGO_ERROR_INVALID_ARGUMENT)
        );
    }

    /// A NaN axis would reach content's movement arithmetic with no way back.
    #[test]
    fn a_non_finite_axis_value_or_timestamp_is_rejected() {
        for bad in [f32::NAN, f32::INFINITY] {
            let axes = [bad];
            assert_eq!(
                unsafe { to_gamepad_state(&state_event(&axes, &[])) }.err(),
                Some(MIGO_ERROR_INVALID_ARGUMENT)
            );
        }
        let mut event = state_event(&[], &[]);
        event.timestamp_ms = f64::NAN;
        assert_eq!(
            unsafe { to_gamepad_state(&event) }.err(),
            Some(MIGO_ERROR_INVALID_ARGUMENT)
        );
    }

    /// A pad with no axes and no buttons is degenerate but not malformed, and
    /// the null pointers that come with it must not be dereferenced.
    #[test]
    fn an_empty_sample_with_null_arrays_is_accepted() {
        let mut event = state_event(&[], &[]);
        event.axes = std::ptr::null();
        event.buttons = std::ptr::null();
        let state = unsafe { to_gamepad_state(&event) }.expect("an empty sample is well-formed");
        assert_eq!(state.axis_count, 0);
        assert_eq!(state.button_count, 0);
    }

    #[test]
    fn a_sample_announcing_entries_it_does_not_supply_is_rejected() {
        let mut event = state_event(&[1.0], &[]);
        event.axes = std::ptr::null();
        assert_eq!(
            unsafe { to_gamepad_state(&event) }.err(),
            Some(MIGO_ERROR_INVALID_ARGUMENT)
        );
    }
}
