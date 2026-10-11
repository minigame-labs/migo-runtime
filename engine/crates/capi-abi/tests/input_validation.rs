use std::{ffi::CString, mem::size_of, os::raw::c_char};

use migo_capi_abi::{
    MIGO_ABI_VERSION_CURRENT, MIGO_ERROR_INVALID_ARGUMENT, VersionedHeader,
    input::{
        MIGO_COMPOSITION_EVENT_END, MIGO_COMPOSITION_EVENT_START, MIGO_COMPOSITION_EVENT_UPDATE,
        MIGO_GAMEPAD_BUTTON_FLAG_PRESSED, MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED,
        MIGO_GAMEPAD_HAPTICS_DUAL_RUMBLE, MIGO_GAMEPAD_HAPTICS_NONE,
        MIGO_GAMEPAD_HAPTICS_TRIGGER_RUMBLE, MIGO_GAMEPAD_MAX_AXES, MIGO_GAMEPAD_MAX_BUTTONS,
        MIGO_GAMEPAD_MAX_COUNT, MIGO_KEY_EVENT_DOWN, MIGO_KEY_EVENT_UP,
        MIGO_KEYBOARD_EVENT_COMPLETE, MIGO_KEYBOARD_EVENT_CONFIRM,
        MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, MIGO_KEYBOARD_EVENT_INPUT, MIGO_TOUCH_CANCEL,
        MIGO_TOUCH_END, MIGO_TOUCH_FLAG_CHANGED, MIGO_TOUCH_FLAG_REMOVED, MIGO_TOUCH_MAX_POINTS,
        MIGO_TOUCH_MOVE, MIGO_TOUCH_START, MigoCompositionEvent, MigoGamepadButton,
        MigoGamepadInfo, MigoGamepadStateEvent, MigoKeyEvent, MigoKeyboardEvent, MigoTouchEvent,
        MigoTouchPoint, ValidatedCompositionEvent, ValidatedGamepadConnection, ValidatedKeyEvent,
        ValidatedKeyboardEvent,
    },
};

fn header<T>() -> VersionedHeader {
    VersionedHeader {
        struct_size: size_of::<T>() as u32,
        abi_version: MIGO_ABI_VERSION_CURRENT,
    }
}

fn touch_event(points: &[MigoTouchPoint], touch_type: u32) -> MigoTouchEvent {
    MigoTouchEvent {
        header: header::<MigoTouchEvent>(),
        touch_type,
        point_count: points.len() as u32,
        timestamp_ms: i64::MIN,
        points: points.as_ptr(),
    }
}

fn point(flags: u32, pressure: f32) -> MigoTouchPoint {
    MigoTouchPoint {
        id: 7,
        x: -12.5,
        y: 42.25,
        pressure,
        flags,
    }
}

#[test]
fn touch_accepts_every_kind_full_timestamp_domain_and_the_ten_point_limit() {
    let points = [point(MIGO_TOUCH_FLAG_CHANGED, 0.5); MIGO_TOUCH_MAX_POINTS];
    for touch_type in [
        MIGO_TOUCH_START,
        MIGO_TOUCH_MOVE,
        MIGO_TOUCH_END,
        MIGO_TOUCH_CANCEL,
    ] {
        let mut event = touch_event(&points, touch_type);
        for timestamp in [i64::MIN, 0, i64::MAX] {
            event.timestamp_ms = timestamp;
            let validated = unsafe { event.validate() }.expect("valid touch batch");
            assert_eq!(validated.touch_type(), touch_type);
            assert_eq!(validated.timestamp_ms(), timestamp);
            assert_eq!(validated.points().len(), MIGO_TOUCH_MAX_POINTS);
        }
    }
}

#[test]
fn touch_requires_one_to_ten_points_and_a_matching_pointer() {
    let points = [point(0, 0.0)];
    let mut event = touch_event(&points, MIGO_TOUCH_MOVE);

    event.point_count = 0;
    assert_eq!(
        unsafe { event.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );
    event.point_count = (MIGO_TOUCH_MAX_POINTS + 1) as u32;
    assert_eq!(
        unsafe { event.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );
    event.point_count = 1;
    event.points = std::ptr::null();
    assert_eq!(
        unsafe { event.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );
}

#[test]
fn touch_rejects_unknown_flags_and_non_finite_or_out_of_range_values() {
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let points = [MigoTouchPoint {
            x: bad,
            ..point(0, 0.0)
        }];
        assert_eq!(
            unsafe { touch_event(&points, MIGO_TOUCH_MOVE).validate() }.unwrap_err(),
            MIGO_ERROR_INVALID_ARGUMENT
        );
    }
    for pressure in [-0.01, 1.01, f32::NAN, f32::INFINITY] {
        let points = [point(0, pressure)];
        assert_eq!(
            unsafe { touch_event(&points, MIGO_TOUCH_MOVE).validate() }.unwrap_err(),
            MIGO_ERROR_INVALID_ARGUMENT
        );
    }
    let points = [point(1 << 31, 0.5)];
    assert_eq!(
        unsafe { touch_event(&points, MIGO_TOUCH_MOVE).validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );
    let points = [point(
        MIGO_TOUCH_FLAG_CHANGED | MIGO_TOUCH_FLAG_REMOVED,
        1.0,
    )];
    unsafe { touch_event(&points, MIGO_TOUCH_END).validate() }.expect("known touch flags");
}

fn keyboard_event(kind: u32, bytes: &[u8]) -> MigoKeyboardEvent {
    MigoKeyboardEvent {
        header: header::<MigoKeyboardEvent>(),
        event_type: kind,
        value_length: bytes.len() as u32,
        value_utf8: bytes.as_ptr().cast::<c_char>(),
        height_css_px: 0.0,
    }
}

#[test]
fn keyboard_text_variants_copy_exact_utf8_and_accept_null_for_empty_text() {
    let text = "\u{4f60}\u{597d}".as_bytes();
    let cases = [
        (MIGO_KEYBOARD_EVENT_INPUT, "input"),
        (MIGO_KEYBOARD_EVENT_CONFIRM, "confirm"),
        (MIGO_KEYBOARD_EVENT_COMPLETE, "complete"),
    ];
    for (kind, expected_kind) in cases {
        let validated = unsafe { keyboard_event(kind, text).validate() }.expect("valid text event");
        match validated {
            ValidatedKeyboardEvent::Input(value) => {
                assert_eq!(expected_kind, "input");
                assert_eq!(value, "\u{4f60}\u{597d}");
            }
            ValidatedKeyboardEvent::Confirm(value) => {
                assert_eq!(expected_kind, "confirm");
                assert_eq!(value, "\u{4f60}\u{597d}");
            }
            ValidatedKeyboardEvent::Complete(value) => {
                assert_eq!(expected_kind, "complete");
                assert_eq!(value, "\u{4f60}\u{597d}");
            }
            ValidatedKeyboardEvent::HeightChange(_) => panic!("wrong keyboard variant"),
        }
    }

    let mut empty = keyboard_event(MIGO_KEYBOARD_EVENT_INPUT, &[]);
    empty.value_utf8 = std::ptr::null();
    assert!(matches!(
        unsafe { empty.validate() }.expect("null plus zero length is empty"),
        ValidatedKeyboardEvent::Input(value) if value.is_empty()
    ));
}

#[test]
fn keyboard_checks_event_kind_before_irrelevant_text_and_validates_height() {
    let mut event = keyboard_event(99, &[]);
    event.value_length = 1;
    event.value_utf8 = std::ptr::null();
    assert_eq!(
        unsafe { event.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );

    event.event_type = MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE;
    event.value_length = u32::MAX;
    for height in [0.0, 320.5] {
        event.height_css_px = height;
        assert!(matches!(
            unsafe { event.validate() }.expect("valid height"),
            ValidatedKeyboardEvent::HeightChange(value) if value == height
        ));
    }
    for height in [-0.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        event.height_css_px = height;
        assert_eq!(
            unsafe { event.validate() }.unwrap_err(),
            MIGO_ERROR_INVALID_ARGUMENT
        );
    }
}

#[test]
fn length_delimited_text_requires_a_pointer_and_an_exact_utf8_boundary() {
    let mut event = keyboard_event(MIGO_KEYBOARD_EVENT_INPUT, b"a");
    event.value_utf8 = std::ptr::null();
    assert_eq!(
        unsafe { event.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );

    let text = "\u{4f60}".as_bytes();
    event = keyboard_event(MIGO_KEYBOARD_EVENT_INPUT, text);
    event.value_length = 1;
    assert_eq!(
        unsafe { event.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );
}

fn key_event(kind: u32, key: &[u8], code: &[u8]) -> MigoKeyEvent {
    MigoKeyEvent {
        header: header::<MigoKeyEvent>(),
        event_type: kind,
        key_length: key.len() as u32,
        key_utf8: key.as_ptr().cast::<c_char>(),
        code_utf8: code.as_ptr().cast::<c_char>(),
        code_length: code.len() as u32,
        reserved0: 0,
        timestamp_ms: -1.0,
        modifiers: 0,
        flags: 0,
    }
}

#[test]
fn keys_accept_empty_key_but_require_code_zero_reserved_and_finite_timestamp() {
    for kind in [MIGO_KEY_EVENT_DOWN, MIGO_KEY_EVENT_UP] {
        let mut event = key_event(kind, &[], b"KeyA");
        event.key_utf8 = std::ptr::null();
        let validated = unsafe { event.validate() }.expect("dead key is valid");
        match validated {
            ValidatedKeyEvent::Down { key, code, .. } | ValidatedKeyEvent::Up { key, code, .. } => {
                assert!(key.is_empty());
                assert_eq!(code, "KeyA");
            }
        }
    }

    let mut event = key_event(MIGO_KEY_EVENT_DOWN, b"a", &[]);
    assert_eq!(
        unsafe { event.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );
    event = key_event(MIGO_KEY_EVENT_DOWN, b"a", b"KeyA");
    event.reserved0 = 1;
    assert_eq!(
        unsafe { event.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );
    event.reserved0 = 0;
    event.timestamp_ms = f64::NAN;
    assert_eq!(
        unsafe { event.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );
}

fn composition_event(kind: u32, data: &[u8]) -> MigoCompositionEvent {
    MigoCompositionEvent {
        header: header::<MigoCompositionEvent>(),
        event_type: kind,
        data_length: data.len() as u32,
        data_utf8: data.as_ptr().cast::<c_char>(),
    }
}

#[test]
fn composition_validates_kind_before_copying_and_accepts_empty_end() {
    let mut unknown = composition_event(99, &[]);
    unknown.data_length = 1;
    unknown.data_utf8 = std::ptr::null();
    assert_eq!(
        unsafe { unknown.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );

    for kind in [
        MIGO_COMPOSITION_EVENT_START,
        MIGO_COMPOSITION_EVENT_UPDATE,
        MIGO_COMPOSITION_EVENT_END,
    ] {
        let mut event = composition_event(kind, &[]);
        event.data_utf8 = std::ptr::null();
        let validated = unsafe { event.validate() }.expect("empty composition data");
        assert!(matches!(
            validated,
            ValidatedCompositionEvent::Start(data)
                | ValidatedCompositionEvent::Update(data)
                | ValidatedCompositionEvent::End(data)
                if data.is_empty()
        ));
    }
}

fn gamepad_info(id: &CString, mapping: &CString) -> MigoGamepadInfo {
    MigoGamepadInfo {
        header: header::<MigoGamepadInfo>(),
        index: 2,
        axis_count: 4,
        button_count: 17,
        haptics: MIGO_GAMEPAD_HAPTICS_NONE,
        id_utf8: id.as_ptr(),
        mapping_utf8: mapping.as_ptr(),
    }
}

#[test]
fn gamepad_connection_boolean_haptics_counts_and_mapping_are_strict() {
    let id = CString::new("Industrial Pad").unwrap();
    let standard = CString::new("standard").unwrap();
    let mut info = gamepad_info(&id, &standard);
    assert!(matches!(
        unsafe { info.validate_connection(1) }.expect("connected pad"),
        ValidatedGamepadConnection::Connected { mapping, .. } if mapping == "standard"
    ));
    assert!(matches!(
        unsafe { info.validate_connection(0) }.expect("disconnected pad"),
        ValidatedGamepadConnection::Disconnected { index: 2 }
    ));
    assert_eq!(
        unsafe { info.validate_connection(2) }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );

    // Every defined effect is carried; a bit this library does not define is a
    // host built against a newer header, refused rather than dropped.
    info.haptics = MIGO_GAMEPAD_HAPTICS_DUAL_RUMBLE | MIGO_GAMEPAD_HAPTICS_TRIGGER_RUMBLE;
    assert!(matches!(
        unsafe { info.validate_connection(1) }.expect("a pad with motors"),
        ValidatedGamepadConnection::Connected { haptics: 3, .. }
    ));
    info.haptics = 1 << 2;
    assert_eq!(
        unsafe { info.validate_connection(1) }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );
    // Withdrawing reads only the slot, so the struct a pad connected with works.
    assert!(matches!(
        unsafe { info.validate_connection(0) }.expect("withdrawn"),
        ValidatedGamepadConnection::Disconnected { index: 2 }
    ));
    info.haptics = MIGO_GAMEPAD_HAPTICS_NONE;
    info.index = MIGO_GAMEPAD_MAX_COUNT as u32;
    assert_eq!(
        unsafe { info.validate_connection(1) }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );
    info.index = 2;
    info.axis_count = (MIGO_GAMEPAD_MAX_AXES + 1) as u32;
    assert_eq!(
        unsafe { info.validate_connection(1) }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );

    let proprietary = CString::new("proprietary").unwrap();
    info = gamepad_info(&id, &proprietary);
    assert_eq!(
        unsafe { info.validate_connection(1) }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );
}

fn gamepad_state(axes: &[f32], buttons: &[MigoGamepadButton]) -> MigoGamepadStateEvent {
    MigoGamepadStateEvent {
        header: header::<MigoGamepadStateEvent>(),
        index: 2,
        axis_count: axes.len() as u32,
        button_count: buttons.len() as u32,
        reserved0: 0,
        axes: axes.as_ptr(),
        buttons: buttons.as_ptr(),
        timestamp_ms: 16.5,
    }
}

#[test]
fn gamepad_sample_copies_valid_axes_buttons_and_boolean_flags() {
    let mut axes = [-1.0, 0.0, 1.0];
    let mut buttons = [
        MigoGamepadButton {
            flags: MIGO_GAMEPAD_BUTTON_FLAG_PRESSED | MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED,
            value: 1.0,
        },
        MigoGamepadButton {
            flags: 0,
            value: 0.25,
        },
    ];
    let event = gamepad_state(&axes, &buttons);
    let validated = unsafe { event.validate() }.expect("valid gamepad sample");
    axes.fill(0.5);
    buttons.fill(MigoGamepadButton {
        flags: 0,
        value: 0.0,
    });

    assert_eq!(validated.axes(), &[-1.0, 0.0, 1.0]);
    assert!(validated.buttons()[0].pressed());
    assert!(validated.buttons()[0].touched());
    assert_eq!(validated.buttons()[0].value(), 1.0);
    assert_eq!(validated.timestamp_ms(), 16.5);
}

#[test]
fn gamepad_sample_rejects_invalid_ranges_flags_reserved_and_timestamp() {
    for axis in [-1.01, 1.01, f32::NAN, f32::INFINITY] {
        let axes = [axis];
        assert_eq!(
            unsafe { gamepad_state(&axes, &[]).validate() }.unwrap_err(),
            MIGO_ERROR_INVALID_ARGUMENT
        );
    }
    for value in [-0.01, 1.01, f32::NAN, f32::INFINITY] {
        let buttons = [MigoGamepadButton { flags: 0, value }];
        assert_eq!(
            unsafe { gamepad_state(&[], &buttons).validate() }.unwrap_err(),
            MIGO_ERROR_INVALID_ARGUMENT
        );
    }
    let buttons = [MigoGamepadButton {
        flags: 1 << 31,
        value: 0.5,
    }];
    assert_eq!(
        unsafe { gamepad_state(&[], &buttons).validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );

    let mut event = gamepad_state(&[], &[]);
    event.index = MIGO_GAMEPAD_MAX_COUNT as u32;
    assert_eq!(
        unsafe { event.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );
    event.index = 2;
    event.reserved0 = 1;
    assert_eq!(
        unsafe { event.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );
    event.reserved0 = 0;
    for timestamp in [-0.01, f64::NAN, f64::INFINITY] {
        event.timestamp_ms = timestamp;
        assert_eq!(
            unsafe { event.validate() }.unwrap_err(),
            MIGO_ERROR_INVALID_ARGUMENT
        );
    }
}

#[test]
fn gamepad_sample_requires_arrays_exactly_when_counts_are_nonzero() {
    let mut event = gamepad_state(&[], &[]);
    event.axes = std::ptr::null();
    event.buttons = std::ptr::null();
    unsafe { event.validate() }.expect("zero counts permit null arrays");

    event.axis_count = 1;
    assert_eq!(
        unsafe { event.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );
    event.axis_count = 0;
    event.button_count = 1;
    assert_eq!(
        unsafe { event.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );

    event.button_count = (MIGO_GAMEPAD_MAX_BUTTONS + 1) as u32;
    event.buttons = std::ptr::dangling();
    assert_eq!(
        unsafe { event.validate() }.unwrap_err(),
        MIGO_ERROR_INVALID_ARGUMENT
    );
}
