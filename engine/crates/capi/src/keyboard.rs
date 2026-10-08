//! Keyboard input, inbound.
//!
//! Two capabilities that share a noun and nothing else. The soft keyboard is
//! text: the host owns an IME and reports the field's whole current value, not
//! the keystroke that changed it -- a host that sends only the newly typed
//! character leaves content whose text never grows past one character. A
//! physical key is a discrete press of an identified key. Content reads the two
//! through different listeners.

use shared::protocol::host_cmd::{HostCommand, captured_generation};

use crate::panic_barrier::guard;
use crate::{MigoSession, map_ingress_result, pin_session};
use migo_capi_abi::MigoResult;

pub use migo_capi_abi::input::{MigoCompositionEvent, MigoKeyEvent, MigoKeyboardEvent};
use migo_capi_abi::input::{ValidatedCompositionEvent, ValidatedKeyEvent, ValidatedKeyboardEvent};

#[cfg(test)]
use migo_capi_abi::input::{
    MIGO_COMPOSITION_EVENT_END, MIGO_COMPOSITION_EVENT_START, MIGO_COMPOSITION_EVENT_UPDATE,
    MIGO_KEY_EVENT_DOWN, MIGO_KEY_EVENT_FLAG_REPEAT, MIGO_KEY_EVENT_UP, MIGO_KEY_MODIFIER_ALT,
    MIGO_KEY_MODIFIER_CONTROL, MIGO_KEY_MODIFIER_SHIFT, MIGO_KEYBOARD_EVENT_COMPLETE,
    MIGO_KEYBOARD_EVENT_CONFIRM, MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, MIGO_KEYBOARD_EVENT_INPUT,
};
#[cfg(test)]
use migo_capi_abi::{MIGO_ERROR_INVALID_ARGUMENT, MIGO_ERROR_INVALID_STATE};

/// Translate a validated event into the engine's command.
///
/// Split out of the entry point so every rejection and every arm is reachable
/// from a test without a live session: this is where a host's event either
/// becomes the right engine command or the wrong one, and the entry point's
/// state checks cannot tell those apart.
///
/// `runtime_generation` is the generation the *calling* host observed, taken
/// from the ingress it is about to enqueue on. Stamping it here rather than
/// reading the current one at dispatch is the whole point: an event a host
/// submitted before a restart committed carries the retired value and is
/// dropped, where a value read later would always match.
fn validated_keyboard_to_command(
    event: ValidatedKeyboardEvent,
    runtime_generation: i64,
) -> HostCommand {
    // Through the same helper every other producer uses rather than a bare
    // `Some`: a host that somehow observed a non-positive generation is
    // unfenced, which is delivered, not retired.
    let runtime_generation = captured_generation(runtime_generation);
    match event {
        ValidatedKeyboardEvent::Input(value) => HostCommand::OnKeyboardInput {
            value,
            runtime_generation,
        },
        ValidatedKeyboardEvent::Confirm(value) => HostCommand::OnKeyboardConfirm {
            value,
            runtime_generation,
        },
        ValidatedKeyboardEvent::Complete(value) => HostCommand::OnKeyboardComplete {
            value,
            runtime_generation,
        },
        ValidatedKeyboardEvent::HeightChange(height) => HostCommand::OnKeyboardHeightChange {
            height,
            runtime_generation,
        },
    }
}

/// Testable composition of pure boundary validation and command conversion.
#[cfg(test)]
unsafe fn to_host_command(
    event: &MigoKeyboardEvent,
    runtime_generation: i64,
) -> Result<HostCommand, MigoResult> {
    unsafe { event.validate() }
        .map(|event| validated_keyboard_to_command(event, runtime_generation))
}

/// Translate a validated composition event into the engine's command.
///
fn validated_composition_to_command(event: ValidatedCompositionEvent) -> HostCommand {
    match event {
        ValidatedCompositionEvent::Start(data) => HostCommand::OnCompositionStart { data },
        ValidatedCompositionEvent::Update(data) => HostCommand::OnCompositionUpdate { data },
        ValidatedCompositionEvent::End(data) => HostCommand::OnCompositionEnd { data },
    }
}

#[cfg(test)]
unsafe fn to_composition_command(event: &MigoCompositionEvent) -> Result<HostCommand, MigoResult> {
    unsafe { event.validate() }.map(validated_composition_to_command)
}

/// Deliver one IME composition event to the session's content.
///
/// # Safety
/// `session` must be live; `event` must point at a `MigoCompositionEvent` whose
/// `data_utf8` holds at least `data_length` bytes. Both are borrowed for the
/// duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_send_composition_event(
    session: *mut MigoSession,
    event: *const MigoCompositionEvent,
) -> MigoResult {
    guard("migo_session_send_composition_event", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let command = match unsafe { MigoCompositionEvent::parse(event) } {
            Ok(event) => validated_composition_to_command(event),
            Err(error) => return error,
        };
        let ingress = match session.active_ingress() {
            Ok(ingress) => ingress,
            Err(error) => return error,
        };

        // A dropped END leaves content drawing a preedit that will never be
        // cleared, and no later event corrects it.
        map_ingress_result(
            &session,
            "migo_session_send_composition_event",
            ingress.try_send_composition(command),
        )
    })
}

/// Translate a validated physical-key event into the engine's command.
///
/// # Safety
/// `key_utf8` and `code_utf8` must each point at their announced number of
/// readable bytes.
fn validated_key_to_command(event: ValidatedKeyEvent) -> HostCommand {
    match event {
        ValidatedKeyEvent::Down {
            key,
            code,
            timestamp_ms,
            modifiers,
            repeat,
        } => HostCommand::OnKeyDown {
            key,
            code,
            timestamp_ms,
            modifiers,
            repeat,
        },
        ValidatedKeyEvent::Up {
            key,
            code,
            timestamp_ms,
            modifiers,
            repeat,
        } => HostCommand::OnKeyUp {
            key,
            code,
            timestamp_ms,
            modifiers,
            repeat,
        },
    }
}

#[cfg(test)]
unsafe fn to_key_command(event: &MigoKeyEvent) -> Result<HostCommand, MigoResult> {
    unsafe { event.validate() }.map(validated_key_to_command)
}

/// Deliver one physical key press or release to the session's content.
///
/// # Safety
/// `session` must be live; `event` must point at a `MigoKeyEvent` whose two
/// strings each hold at least their announced length. All are borrowed for the
/// duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_send_key_event(
    session: *mut MigoSession,
    event: *const MigoKeyEvent,
) -> MigoResult {
    guard("migo_session_send_key_event", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let command = match unsafe { MigoKeyEvent::parse(event) } {
            Ok(event) => validated_key_to_command(event),
            Err(error) => return error,
        };
        let ingress = match session.active_ingress() {
            Ok(ingress) => ingress,
            Err(error) => return error,
        };

        // A dropped UP leaves content believing the key is still held, and no
        // later event corrects it.
        map_ingress_result(
            &session,
            "migo_session_send_key_event",
            ingress.try_send_key(command),
        )
    })
}

/// Deliver one soft-keyboard event to the session's content.
///
/// # Safety
/// `session` must be live; `event` must point at a `MigoKeyboardEvent` whose
/// `value_utf8` holds at least `value_length` bytes. Both are borrowed for the
/// duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_send_keyboard_event(
    session: *mut MigoSession,
    event: *const MigoKeyboardEvent,
) -> MigoResult {
    guard("migo_session_send_keyboard_event", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        // Validated before the session lock so a malformed event is rejected on
        // its own terms rather than reporting whatever the session state is.
        let validated = match unsafe { MigoKeyboardEvent::parse(event) } {
            Ok(event) => event,
            Err(error) => return error,
        };

        let ingress = match session.active_ingress() {
            Ok(ingress) => ingress,
            Err(error) => return error,
        };

        // Read from the ingress this event is about to be queued on, so the
        // stamp names the runtime the caller was talking to.
        let command = validated_keyboard_to_command(validated, ingress.runtime_generation());

        // Dropping a COMPLETE would leave content believing the keyboard is
        // still open, and no later event corrects that -- the same reason the
        // touch path refuses to drop an END.
        map_ingress_result(
            &session,
            "migo_session_send_keyboard_event",
            ingress.try_send_keyboard(command),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_session;
    use migo_capi_abi::{MIGO_ABI_VERSION_CURRENT, MIGO_ERROR_UNSUPPORTED_ABI, VersionedHeader};
    use std::mem::size_of;
    use std::num::NonZeroI64;

    fn text_event(kind: u32, value: &str) -> MigoKeyboardEvent {
        MigoKeyboardEvent {
            header: VersionedHeader {
                struct_size: size_of::<MigoKeyboardEvent>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            event_type: kind,
            value_length: value.len() as u32,
            value_utf8: value.as_ptr() as *const std::os::raw::c_char,
            height_css_px: 0.0,
        }
    }

    fn height_event(height: f64) -> MigoKeyboardEvent {
        MigoKeyboardEvent {
            header: VersionedHeader {
                struct_size: size_of::<MigoKeyboardEvent>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            event_type: MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE,
            value_length: 0,
            value_utf8: std::ptr::null(),
            height_css_px: height,
        }
    }

    fn composition_event(kind: u32, data: &str) -> MigoCompositionEvent {
        MigoCompositionEvent {
            header: VersionedHeader {
                struct_size: size_of::<MigoCompositionEvent>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            event_type: kind,
            data_length: data.len() as u32,
            data_utf8: data.as_ptr() as *const std::os::raw::c_char,
        }
    }

    /// Each composition event must reach its own command. Content draws the
    /// preedit on update and clears it on end, so a swapped arm leaves text on
    /// screen that the user already committed or cancelled.
    #[test]
    fn every_composition_type_reaches_its_own_command() {
        let cases = [
            (MIGO_COMPOSITION_EVENT_START, ""),
            (MIGO_COMPOSITION_EVENT_UPDATE, "nihao"),
            (MIGO_COMPOSITION_EVENT_END, "\u{4f60}\u{597d}"),
        ];
        for (kind, data) in cases {
            let event = composition_event(kind, data);
            match (
                kind,
                unsafe { to_composition_command(&event) }.expect("well-formed"),
            ) {
                (MIGO_COMPOSITION_EVENT_START, HostCommand::OnCompositionStart { data: got }) => {
                    assert_eq!(got, data)
                }
                (MIGO_COMPOSITION_EVENT_UPDATE, HostCommand::OnCompositionUpdate { data: got }) => {
                    assert_eq!(got, data)
                }
                (MIGO_COMPOSITION_EVENT_END, HostCommand::OnCompositionEnd { data: got }) => {
                    assert_eq!(got, data)
                }
                (kind, other) => panic!("composition type {kind} produced {other:?}"),
            }
        }
    }

    /// Multi-byte preedit text is the whole point of composition, so a length
    /// that splits a character must be refused rather than delivered mangled.
    #[test]
    fn a_composition_length_that_splits_a_character_is_rejected() {
        let data = "\u{4f60}\u{597d}";
        let mut event = composition_event(MIGO_COMPOSITION_EVENT_UPDATE, data);
        event.data_length = 1; // half of the first code point
        assert_eq!(
            unsafe { to_composition_command(&event) }.err(),
            Some(MIGO_ERROR_INVALID_ARGUMENT)
        );
    }

    /// An empty end is a cancelled composition, which content must still see so
    /// it can clear the preedit it has been drawing.
    #[test]
    fn an_empty_composition_end_is_valid_because_it_means_cancelled() {
        let event = composition_event(MIGO_COMPOSITION_EVENT_END, "");
        match unsafe { to_composition_command(&event) }.expect("well-formed") {
            HostCommand::OnCompositionEnd { data } => assert_eq!(data, ""),
            other => panic!("expected a composition end, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_composition_type_is_rejected() {
        assert_eq!(
            unsafe { to_composition_command(&composition_event(99, "x")) }.err(),
            Some(MIGO_ERROR_INVALID_ARGUMENT)
        );
    }

    #[test]
    fn send_composition_without_an_attached_surface_reports_invalid_state() {
        with_session("composition-detached", |session| {
            let event = composition_event(MIGO_COMPOSITION_EVENT_UPDATE, "ni");
            assert_eq!(
                unsafe { migo_session_send_composition_event(session, &event) },
                MIGO_ERROR_INVALID_STATE
            );
        });
    }

    fn key_event(kind: u32, key: &str, code: &str) -> MigoKeyEvent {
        MigoKeyEvent {
            header: VersionedHeader {
                struct_size: size_of::<MigoKeyEvent>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            event_type: kind,
            key_length: key.len() as u32,
            key_utf8: key.as_ptr() as *const std::os::raw::c_char,
            code_utf8: code.as_ptr() as *const std::os::raw::c_char,
            code_length: code.len() as u32,
            reserved0: 0,
            timestamp_ms: 1234.0,
            modifiers: 0,
            flags: 0,
        }
    }

    /// A host built before modifiers existed must still be accepted.
    ///
    /// It announces the smaller `struct_size` and never allocated the tail, so
    /// the bytes past it are poisoned here: a library that read them would see
    /// 0xAAAAAAAA as a modifier mask, fail the known-bits check, and reject a
    /// perfectly valid old caller -- or, worse, accept it and hand content four
    /// modifiers nobody was holding.
    #[test]
    fn a_client_from_before_modifiers_is_accepted_and_reads_as_none() {
        const OLD_SIZE: usize = std::mem::offset_of!(MigoKeyEvent, modifiers);
        let key = "s";
        let code = "KeyS";
        let mut full = key_event(MIGO_KEY_EVENT_DOWN, key, code);
        full.header.struct_size = OLD_SIZE as u32;
        // Deliberately VALID bits rather than garbage. Garbage past the
        // announced size would be caught by the known-bits check and surface as
        // an error, which is the easy failure; a plausible value surfaces as
        // content being told Ctrl and Alt are held when the host said nothing
        // at all, which nothing downstream can detect.
        full.modifiers = MIGO_KEY_MODIFIER_CONTROL | MIGO_KEY_MODIFIER_ALT;
        full.flags = MIGO_KEY_EVENT_FLAG_REPEAT;

        let mut buffer = [0u8; size_of::<MigoKeyEvent>()];
        // SAFETY: both regions are exactly `size_of::<MigoKeyEvent>()` bytes of
        // plain data, and `buffer` is written, not read, through this pointer.
        unsafe {
            std::ptr::copy_nonoverlapping(
                &full as *const MigoKeyEvent as *const u8,
                buffer.as_mut_ptr(),
                size_of::<MigoKeyEvent>(),
            );
        }

        // SAFETY: the buffer holds a well-formed prefix of the announced size,
        // and both strings outlive the call.
        let command = unsafe { MigoKeyEvent::parse(buffer.as_ptr() as *const MigoKeyEvent) }
            .map(validated_key_to_command)
            .expect("a caller from the previous header must be accepted");
        match command {
            HostCommand::OnKeyDown {
                modifiers, repeat, ..
            } => {
                assert_eq!(modifiers, 0, "an absent tail must read as no modifiers");
                assert!(!repeat, "an absent tail must not read as an auto-repeat");
            }
            other => panic!("expected OnKeyDown, got {other:?}"),
        }
    }

    /// Modifiers and repeat must survive the boundary, and unknown bits must not.
    #[test]
    fn modifiers_and_repeat_cross_the_boundary_and_unknown_bits_are_refused() {
        let mut event = key_event(MIGO_KEY_EVENT_DOWN, "s", "KeyS");
        event.modifiers = MIGO_KEY_MODIFIER_CONTROL | MIGO_KEY_MODIFIER_SHIFT;
        event.flags = MIGO_KEY_EVENT_FLAG_REPEAT;
        match unsafe { to_key_command(&event) }.expect("well-formed") {
            HostCommand::OnKeyDown {
                modifiers, repeat, ..
            } => {
                assert_eq!(
                    modifiers,
                    MIGO_KEY_MODIFIER_CONTROL | MIGO_KEY_MODIFIER_SHIFT
                );
                assert!(repeat);
            }
            other => panic!("expected OnKeyDown, got {other:?}"),
        }

        // A bit this version does not define may mean something in a later one;
        // accepting it now would let a host rely on being ignored.
        let mut unknown = key_event(MIGO_KEY_EVENT_DOWN, "s", "KeyS");
        unknown.modifiers = 1 << 16;
        assert_eq!(
            unsafe { to_key_command(&unknown) }.err(),
            Some(MIGO_ERROR_INVALID_ARGUMENT)
        );
    }

    /// `key` and `code` are different values and must not be swapped: a host
    /// that sends `code` in both leaves content reading "KeyA" as typed text.
    /// Distinct values on each side are what makes a swap show up here rather
    /// than in a game.
    #[test]
    fn down_and_up_carry_key_and_code_in_the_right_places() {
        for (kind, expect_down) in [(MIGO_KEY_EVENT_DOWN, true), (MIGO_KEY_EVENT_UP, false)] {
            let event = key_event(kind, "a", "KeyA");
            match unsafe { to_key_command(&event) }.expect("well-formed") {
                HostCommand::OnKeyDown {
                    key,
                    code,
                    timestamp_ms,
                    ..
                } => {
                    assert!(expect_down, "a down arm produced an up command");
                    assert_eq!(key, "a");
                    assert_eq!(code, "KeyA");
                    assert_eq!(timestamp_ms, 1234.0);
                }
                HostCommand::OnKeyUp { key, code, .. } => {
                    assert!(!expect_down, "an up arm produced a down command");
                    assert_eq!(key, "a");
                    assert_eq!(code, "KeyA");
                }
                other => panic!("expected a key command, got {other:?}"),
            }
        }
    }

    /// A dead key produces no text, so an empty `key` is a real event.
    #[test]
    fn an_empty_key_is_valid_because_some_keys_produce_no_text() {
        let event = key_event(MIGO_KEY_EVENT_DOWN, "", "Backquote");
        match unsafe { to_key_command(&event) }.expect("well-formed") {
            HostCommand::OnKeyDown { key, code, .. } => {
                assert_eq!(key, "");
                assert_eq!(code, "Backquote");
            }
            other => panic!("expected a key down, got {other:?}"),
        }
    }

    /// `code` always identifies a physical key, so an empty one means the host
    /// did not fill it in -- not that the key has no identity.
    #[test]
    fn an_empty_code_is_rejected() {
        let event = key_event(MIGO_KEY_EVENT_DOWN, "a", "");
        assert_eq!(
            unsafe { to_key_command(&event) }.err(),
            Some(MIGO_ERROR_INVALID_ARGUMENT)
        );
    }

    #[test]
    fn an_unknown_key_event_type_is_rejected() {
        assert_eq!(
            unsafe { to_key_command(&key_event(99, "a", "KeyA")) }.err(),
            Some(MIGO_ERROR_INVALID_ARGUMENT)
        );
    }

    #[test]
    fn a_non_finite_key_timestamp_is_rejected() {
        let mut event = key_event(MIGO_KEY_EVENT_DOWN, "a", "KeyA");
        event.timestamp_ms = f64::NAN;
        assert_eq!(
            unsafe { to_key_command(&event) }.err(),
            Some(MIGO_ERROR_INVALID_ARGUMENT)
        );
    }

    #[test]
    fn a_key_event_with_a_null_string_is_rejected() {
        let mut null_key = key_event(MIGO_KEY_EVENT_DOWN, "a", "KeyA");
        null_key.key_utf8 = std::ptr::null();
        assert_eq!(
            unsafe { to_key_command(&null_key) }.err(),
            Some(MIGO_ERROR_INVALID_ARGUMENT)
        );

        let mut null_code = key_event(MIGO_KEY_EVENT_DOWN, "a", "KeyA");
        null_code.code_utf8 = std::ptr::null();
        assert_eq!(
            unsafe { to_key_command(&null_code) }.err(),
            Some(MIGO_ERROR_INVALID_ARGUMENT)
        );
    }

    /// Only the announced lengths are read: a host may hold either string in a
    /// larger buffer with no NUL, and reading past would leak its memory into
    /// content.
    #[test]
    fn only_the_announced_key_lengths_are_read() {
        let mut event = key_event(MIGO_KEY_EVENT_DOWN, "abcdef", "KeyAxxxx");
        event.key_length = 1;
        event.code_length = 4;
        match unsafe { to_key_command(&event) }.expect("well-formed") {
            HostCommand::OnKeyDown { key, code, .. } => {
                assert_eq!(key, "a");
                assert_eq!(code, "KeyA");
            }
            other => panic!("expected a key down, got {other:?}"),
        }
    }

    #[test]
    fn send_key_without_an_attached_surface_reports_invalid_state() {
        with_session("key-detached", |session| {
            let event = key_event(MIGO_KEY_EVENT_DOWN, "a", "KeyA");
            assert_eq!(
                unsafe { migo_session_send_key_event(session, &event) },
                MIGO_ERROR_INVALID_STATE
            );
        });
    }

    #[test]
    fn send_key_rejects_a_null_session_and_a_null_event() {
        let event = key_event(MIGO_KEY_EVENT_DOWN, "a", "KeyA");
        assert_eq!(
            unsafe { migo_session_send_key_event(std::ptr::null_mut(), &event) },
            MIGO_ERROR_INVALID_ARGUMENT
        );
        with_session("key-null-event", |session| {
            assert_eq!(
                unsafe { migo_session_send_key_event(session, std::ptr::null()) },
                MIGO_ERROR_INVALID_ARGUMENT
            );
        });
    }

    /// Each event type must reach the matching engine command carrying its
    /// value. A swapped arm here would deliver a confirm as an input, which
    /// content sees as a field that never commits.
    #[test]
    fn every_event_type_reaches_its_own_command() {
        let cases = [
            (MIGO_KEYBOARD_EVENT_INPUT, "hello"),
            (MIGO_KEYBOARD_EVENT_CONFIRM, "hello"),
            (MIGO_KEYBOARD_EVENT_COMPLETE, "hello"),
        ];
        for (kind, value) in cases {
            let event = text_event(kind, value);
            let command = unsafe { to_host_command(&event, 1) }.expect("well-formed");
            match (kind, command) {
                (MIGO_KEYBOARD_EVENT_INPUT, HostCommand::OnKeyboardInput { value: got, .. }) => {
                    assert_eq!(got, value)
                }
                (
                    MIGO_KEYBOARD_EVENT_CONFIRM,
                    HostCommand::OnKeyboardConfirm { value: got, .. },
                ) => {
                    assert_eq!(got, value)
                }
                (
                    MIGO_KEYBOARD_EVENT_COMPLETE,
                    HostCommand::OnKeyboardComplete { value: got, .. },
                ) => {
                    assert_eq!(got, value)
                }
                (kind, other) => panic!("event type {kind} produced {other:?}"),
            }
        }
    }

    #[test]
    fn soft_keyboard_commands_capture_ingress_generation() {
        // Every one of the four, because the conversion builds each arm
        // separately and a stamp is exactly the kind of thing that gets added to
        // three of four. The value is the caller's, not a re-read: a keyboard
        // event submitted before a restart commits must carry the retired
        // generation so the Host drops it.
        let events = [
            text_event(MIGO_KEYBOARD_EVENT_INPUT, "hello"),
            text_event(MIGO_KEYBOARD_EVENT_CONFIRM, "hello"),
            text_event(MIGO_KEYBOARD_EVENT_COMPLETE, "hello"),
            height_event(320.5),
        ];
        for event in &events {
            let command = unsafe { to_host_command(event, 7) }.expect("well-formed");
            assert_eq!(
                command.callback_generation(),
                NonZeroI64::new(7),
                "{command:?} lost its generation"
            );
        }

        // A second value, so a hard-coded constant cannot pass the first half.
        let command = unsafe { to_host_command(&events[0], 8) }.expect("well-formed");
        assert_eq!(command.callback_generation(), NonZeroI64::new(8));
    }

    #[test]
    fn a_height_change_carries_its_height() {
        let event = height_event(320.5);
        match unsafe { to_host_command(&event, 1) }.expect("well-formed") {
            HostCommand::OnKeyboardHeightChange { height, .. } => assert_eq!(height, 320.5),
            other => panic!("expected a height change, got {other:?}"),
        }
    }

    /// Zero is how a host reports the keyboard closing, so it must convert.
    #[test]
    fn a_zero_height_is_valid_because_it_means_dismissed() {
        assert!(unsafe { to_host_command(&height_event(0.0), 1) }.is_ok());
    }

    /// NaN would reach content's layout arithmetic with no way back out.
    #[test]
    fn a_non_finite_or_negative_height_is_rejected() {
        for bad in [f64::NAN, f64::INFINITY, -1.0] {
            assert_eq!(
                unsafe { to_host_command(&height_event(bad), 1) }.err(),
                Some(MIGO_ERROR_INVALID_ARGUMENT),
                "height {bad} must be rejected"
            );
        }
    }

    /// An empty value is the content's field being cleared, which is a real
    /// event and not a malformed one.
    #[test]
    fn an_empty_value_is_valid() {
        let event = text_event(MIGO_KEYBOARD_EVENT_INPUT, "");
        match unsafe { to_host_command(&event, 1) }.expect("well-formed") {
            HostCommand::OnKeyboardInput { value, .. } => assert_eq!(value, ""),
            other => panic!("expected an input, got {other:?}"),
        }
    }

    /// The value is length-delimited, so a host may supply a buffer that is not
    /// NUL-terminated and one that has more bytes after the announced length.
    /// Reading past `value_length` would leak the host's adjacent memory into
    /// content.
    #[test]
    fn only_the_announced_length_is_read() {
        let backing = "abcXXXXX";
        let mut event = text_event(MIGO_KEYBOARD_EVENT_INPUT, backing);
        event.value_length = 3;
        match unsafe { to_host_command(&event, 1) }.expect("well-formed") {
            HostCommand::OnKeyboardInput { value, .. } => assert_eq!(value, "abc"),
            other => panic!("expected an input, got {other:?}"),
        }
    }

    /// Mangling what a user typed is worse than an error the host can see.
    #[test]
    fn an_invalid_utf8_value_is_rejected() {
        let bytes: [u8; 3] = [0x61, 0xff, 0x62];
        let mut event = text_event(MIGO_KEYBOARD_EVENT_INPUT, "abc");
        event.value_utf8 = bytes.as_ptr() as *const std::os::raw::c_char;
        event.value_length = 3;
        assert_eq!(
            unsafe { to_host_command(&event, 1) }.err(),
            Some(MIGO_ERROR_INVALID_ARGUMENT)
        );
    }

    #[test]
    fn a_text_event_with_a_null_value_is_rejected() {
        let mut event = text_event(MIGO_KEYBOARD_EVENT_INPUT, "abc");
        event.value_utf8 = std::ptr::null();
        assert_eq!(
            unsafe { to_host_command(&event, 1) }.err(),
            Some(MIGO_ERROR_INVALID_ARGUMENT)
        );
    }

    #[test]
    fn an_unknown_event_type_is_rejected() {
        let event = text_event(99, "abc");
        assert_eq!(
            unsafe { to_host_command(&event, 1) }.err(),
            Some(MIGO_ERROR_INVALID_ARGUMENT)
        );
    }

    #[test]
    fn send_rejects_a_null_session() {
        let event = text_event(MIGO_KEYBOARD_EVENT_INPUT, "abc");
        assert_eq!(
            unsafe { migo_session_send_keyboard_event(std::ptr::null_mut(), &event) },
            MIGO_ERROR_INVALID_ARGUMENT
        );
    }

    #[test]
    fn send_rejects_a_null_event() {
        with_session("keyboard-null-event", |session| {
            assert_eq!(
                unsafe { migo_session_send_keyboard_event(session, std::ptr::null()) },
                MIGO_ERROR_INVALID_ARGUMENT
            );
        });
    }

    #[test]
    fn send_rejects_a_struct_size_mismatch() {
        with_session("keyboard-size", |session| {
            let mut event = text_event(MIGO_KEYBOARD_EVENT_INPUT, "abc");
            event.header.struct_size = 8;
            assert_eq!(
                unsafe { migo_session_send_keyboard_event(session, &event) },
                MIGO_ERROR_INVALID_ARGUMENT
            );
        });
    }

    #[test]
    fn send_rejects_an_unknown_abi_version() {
        with_session("keyboard-abi", |session| {
            let mut event = text_event(MIGO_KEYBOARD_EVENT_INPUT, "abc");
            event.header.abi_version = 99;
            assert_eq!(
                unsafe { migo_session_send_keyboard_event(session, &event) },
                MIGO_ERROR_UNSUPPORTED_ABI
            );
        });
    }

    /// Reporting success with nothing attached would tell the host its event
    /// was delivered when there is no content to deliver it to.
    #[test]
    fn send_without_an_attached_surface_reports_invalid_state() {
        with_session("keyboard-detached", |session| {
            let event = text_event(MIGO_KEYBOARD_EVENT_INPUT, "abc");
            assert_eq!(
                unsafe { migo_session_send_keyboard_event(session, &event) },
                MIGO_ERROR_INVALID_STATE
            );
        });
    }
}
