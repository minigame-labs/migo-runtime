//! Allocation-bounded validation for every public input record.
//!
//! Raw ABI values are copied before validation. Borrowed strings and arrays are
//! consumed during the call and never retained. Touch and gamepad samples use
//! fixed inline storage; only the text-bearing variants allocate.

use std::{
    mem::{offset_of, size_of},
    os::raw::c_char,
};

use crate::{
    AbiStruct, MIGO_ERROR_INVALID_ARGUMENT, MigoResult, VersionedHeader, copy_utf8,
    copy_utf8_with_length, copy_versioned,
    validate::{validate_f32_range, validate_f64_range, validate_flags, validate_reserved},
};

pub const MIGO_TOUCH_START: u32 = 0;
pub const MIGO_TOUCH_MOVE: u32 = 1;
pub const MIGO_TOUCH_END: u32 = 2;
pub const MIGO_TOUCH_CANCEL: u32 = 3;
pub const MIGO_TOUCH_FLAG_CHANGED: u32 = 1 << 0;
pub const MIGO_TOUCH_FLAG_REMOVED: u32 = 1 << 1;
const MIGO_TOUCH_FLAG_KNOWN_MASK: u32 = MIGO_TOUCH_FLAG_CHANGED | MIGO_TOUCH_FLAG_REMOVED;
pub const MIGO_TOUCH_MAX_POINTS: usize = 10;

pub const MIGO_POINTER_EVENT_DOWN: u32 = 0;
pub const MIGO_POINTER_EVENT_MOVE: u32 = 1;
pub const MIGO_POINTER_EVENT_UP: u32 = 2;
/// Highest accepted `MouseEvent.button` ordinal.
///
/// The DOM defines five, and content switches on the value. The bound is well
/// past them so a real device is never refused, while an uninitialised field
/// still fails instead of reaching content as a button that cannot exist.
pub const MIGO_POINTER_BUTTON_MAX: u32 = 31;

pub const MIGO_WHEEL_DELTA_MODE_PIXEL: u32 = 0;
pub const MIGO_WHEEL_DELTA_MODE_LINE: u32 = 1;
pub const MIGO_WHEEL_DELTA_MODE_PAGE: u32 = 2;

pub const MIGO_KEYBOARD_EVENT_INPUT: u32 = 0;
pub const MIGO_KEYBOARD_EVENT_CONFIRM: u32 = 1;
pub const MIGO_KEYBOARD_EVENT_COMPLETE: u32 = 2;
pub const MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE: u32 = 3;

pub const MIGO_KEY_EVENT_DOWN: u32 = 0;
pub const MIGO_KEY_EVENT_UP: u32 = 1;

/// DOM `KeyboardEvent` modifier state, as a bitmask.
///
/// Content cannot tell `Ctrl+S` from `S` without it, and `key` alone does not
/// carry it: a modified press still reports the character it produces.
pub const MIGO_KEY_MODIFIER_CONTROL: u32 = 1 << 0;
pub const MIGO_KEY_MODIFIER_SHIFT: u32 = 1 << 1;
pub const MIGO_KEY_MODIFIER_ALT: u32 = 1 << 2;
pub const MIGO_KEY_MODIFIER_META: u32 = 1 << 3;
const MIGO_KEY_MODIFIER_KNOWN_MASK: u32 = MIGO_KEY_MODIFIER_CONTROL
    | MIGO_KEY_MODIFIER_SHIFT
    | MIGO_KEY_MODIFIER_ALT
    | MIGO_KEY_MODIFIER_META;

/// This press was produced by the platform's auto-repeat, not by the user
/// pressing the key again. DOM `KeyboardEvent.repeat`.
pub const MIGO_KEY_EVENT_FLAG_REPEAT: u32 = 1 << 0;
const MIGO_KEY_EVENT_FLAG_KNOWN_MASK: u32 = MIGO_KEY_EVENT_FLAG_REPEAT;

pub const MIGO_COMPOSITION_EVENT_START: u32 = 0;
pub const MIGO_COMPOSITION_EVENT_UPDATE: u32 = 1;
pub const MIGO_COMPOSITION_EVENT_END: u32 = 2;

pub const MIGO_GAMEPAD_MAX_AXES: usize = 8;
pub const MIGO_GAMEPAD_MAX_BUTTONS: usize = 20;
/// Stable Web Gamepad slots exposed by one Session.
///
/// Keeping this finite makes both boundary validation and the Session's
/// topology table allocation-bounded. Indices are never compacted: a host
/// must keep a physical device in the same slot until it disconnects.
pub const MIGO_GAMEPAD_MAX_COUNT: usize = 16;
pub const MIGO_GAMEPAD_BUTTON_FLAG_PRESSED: u32 = 1 << 0;
pub const MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED: u32 = 1 << 1;
const MIGO_GAMEPAD_BUTTON_FLAG_KNOWN_MASK: u32 =
    MIGO_GAMEPAD_BUTTON_FLAG_PRESSED | MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MigoTouchPoint {
    pub id: u32,
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoTouchEvent {
    pub header: VersionedHeader,
    pub touch_type: u32,
    pub point_count: u32,
    pub timestamp_ms: i64,
    pub points: *const MigoTouchPoint,
}

// SAFETY: this repr(C) record contains only zero-valid scalars and a pointer;
// v1 requires the complete record.
unsafe impl AbiStruct for MigoTouchEvent {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValidatedTouchEvent {
    touch_type: u32,
    point_count: u8,
    points: [MigoTouchPoint; MIGO_TOUCH_MAX_POINTS],
    timestamp_ms: i64,
}

impl ValidatedTouchEvent {
    #[inline]
    pub const fn touch_type(&self) -> u32 {
        self.touch_type
    }

    #[inline]
    pub fn points(&self) -> &[MigoTouchPoint] {
        &self.points[..self.point_count as usize]
    }

    #[inline]
    pub const fn timestamp_ms(&self) -> i64 {
        self.timestamp_ms
    }

    #[inline]
    pub const fn into_parts(self) -> (u32, u8, [MigoTouchPoint; MIGO_TOUCH_MAX_POINTS], i64) {
        (
            self.touch_type,
            self.point_count,
            self.points,
            self.timestamp_ms,
        )
    }
}

impl MigoTouchEvent {
    /// # Safety
    /// `event` must be readable for its announced size; when point_count is
    /// non-zero, `points` must reference that many readable entries.
    pub unsafe fn parse(event: *const Self) -> Result<ValidatedTouchEvent, MigoResult> {
        // SAFETY: forwarded from this method's contract.
        let raw = unsafe { copy_versioned::<Self>(event.cast()) }?;
        // SAFETY: the copied pointer retains call-duration validity.
        unsafe { raw.validate_fields() }
    }

    /// # Safety
    /// When point_count is non-zero, `points` must reference that many readable
    /// entries for this call.
    pub unsafe fn validate(&self) -> Result<ValidatedTouchEvent, MigoResult> {
        // SAFETY: Self is complete and the caller supplies the pointee contract.
        let raw = unsafe { copy_versioned::<Self>((self as *const Self).cast()) }?;
        unsafe { raw.validate_fields() }
    }

    unsafe fn validate_fields(self) -> Result<ValidatedTouchEvent, MigoResult> {
        match self.touch_type {
            MIGO_TOUCH_START | MIGO_TOUCH_MOVE | MIGO_TOUCH_END | MIGO_TOUCH_CANCEL => {}
            _ => return Err(MIGO_ERROR_INVALID_ARGUMENT),
        }
        let point_count = self.point_count as usize;
        if !(1..=MIGO_TOUCH_MAX_POINTS).contains(&point_count) || self.points.is_null() {
            return Err(MIGO_ERROR_INVALID_ARGUMENT);
        }

        let mut points = [MigoTouchPoint::default(); MIGO_TOUCH_MAX_POINTS];
        // SAFETY: the method contract and bounds above establish this slice.
        let source = unsafe { std::slice::from_raw_parts(self.points, point_count) };
        for point in source {
            if !point.x.is_finite() || !point.y.is_finite() {
                return Err(MIGO_ERROR_INVALID_ARGUMENT);
            }
            validate_f32_range(point.pressure, 0.0..=1.0)?;
            validate_flags(point.flags.into(), MIGO_TOUCH_FLAG_KNOWN_MASK.into())?;
        }
        points[..point_count].copy_from_slice(source);

        Ok(ValidatedTouchEvent {
            touch_type: self.touch_type,
            point_count: point_count as u8,
            points,
            timestamp_ms: self.timestamp_ms,
        })
    }
}

/// One mouse button or motion event.
///
/// Carries no pointer, so its layout is identical on LP64, LLP64 and ILP32 --
/// unlike the string-bearing records, where the two halves of every layout
/// assertion really do differ.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoPointerEvent {
    pub header: VersionedHeader,
    pub event_type: u32,
    pub button: u32,
    pub x: f32,
    pub y: f32,
    pub timestamp_ms: f64,
}

// SAFETY: all fields are zero-valid and v1 requires the complete record.
unsafe impl AbiStruct for MigoPointerEvent {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValidatedPointerEvent {
    pub event_type: u32,
    pub button: u32,
    pub x: f32,
    pub y: f32,
    pub timestamp_ms: f64,
}

impl MigoPointerEvent {
    /// # Safety
    /// `event` must be readable for its announced size.
    pub unsafe fn parse(event: *const Self) -> Result<ValidatedPointerEvent, MigoResult> {
        let raw = unsafe { copy_versioned::<Self>(event.cast()) }?;
        raw.validate_fields()
    }

    pub fn validate(&self) -> Result<ValidatedPointerEvent, MigoResult> {
        // SAFETY: Self is complete and readable through this borrow.
        let raw = unsafe { copy_versioned::<Self>((self as *const Self).cast()) }?;
        raw.validate_fields()
    }

    fn validate_fields(self) -> Result<ValidatedPointerEvent, MigoResult> {
        match self.event_type {
            MIGO_POINTER_EVENT_DOWN | MIGO_POINTER_EVENT_MOVE | MIGO_POINTER_EVENT_UP => {}
            _ => return Err(MIGO_ERROR_INVALID_ARGUMENT),
        }
        if self.button > MIGO_POINTER_BUTTON_MAX {
            return Err(MIGO_ERROR_INVALID_ARGUMENT);
        }
        // Same rule the touch path applies to a point: a non-finite coordinate
        // reaches content as NaN and silently removes whatever it hit-tests.
        if !self.x.is_finite() || !self.y.is_finite() || !self.timestamp_ms.is_finite() {
            return Err(MIGO_ERROR_INVALID_ARGUMENT);
        }
        Ok(ValidatedPointerEvent {
            event_type: self.event_type,
            button: self.button,
            x: self.x,
            y: self.y,
            timestamp_ms: self.timestamp_ms,
        })
    }
}

/// One wheel or trackpad scroll event.
///
/// Also pointer-free, so it too has one layout on every target.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoWheelEvent {
    pub header: VersionedHeader,
    pub delta_mode: u32,
    pub reserved0: u32,
    pub delta_x: f64,
    pub delta_y: f64,
    pub delta_z: f64,
    pub timestamp_ms: f64,
}

// SAFETY: all fields are zero-valid and v1 requires the complete record.
unsafe impl AbiStruct for MigoWheelEvent {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValidatedWheelEvent {
    pub delta_x: f64,
    pub delta_y: f64,
    pub delta_z: f64,
    pub delta_mode: u32,
    pub timestamp_ms: f64,
}

impl MigoWheelEvent {
    /// # Safety
    /// `event` must be readable for its announced size.
    pub unsafe fn parse(event: *const Self) -> Result<ValidatedWheelEvent, MigoResult> {
        let raw = unsafe { copy_versioned::<Self>(event.cast()) }?;
        raw.validate_fields()
    }

    pub fn validate(&self) -> Result<ValidatedWheelEvent, MigoResult> {
        // SAFETY: Self is complete and readable through this borrow.
        let raw = unsafe { copy_versioned::<Self>((self as *const Self).cast()) }?;
        raw.validate_fields()
    }

    fn validate_fields(self) -> Result<ValidatedWheelEvent, MigoResult> {
        match self.delta_mode {
            MIGO_WHEEL_DELTA_MODE_PIXEL
            | MIGO_WHEEL_DELTA_MODE_LINE
            | MIGO_WHEEL_DELTA_MODE_PAGE => {}
            // An unknown mode is refused rather than assumed to be pixels: a
            // line-sized number read as pixels scrolls by a hundredth of what
            // the user asked for, and nothing later corrects it.
            _ => return Err(MIGO_ERROR_INVALID_ARGUMENT),
        }
        validate_reserved(self.reserved0.into())?;
        if !self.delta_x.is_finite()
            || !self.delta_y.is_finite()
            || !self.delta_z.is_finite()
            || !self.timestamp_ms.is_finite()
        {
            return Err(MIGO_ERROR_INVALID_ARGUMENT);
        }
        Ok(ValidatedWheelEvent {
            delta_x: self.delta_x,
            delta_y: self.delta_y,
            delta_z: self.delta_z,
            delta_mode: self.delta_mode,
            timestamp_ms: self.timestamp_ms,
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoKeyboardEvent {
    pub header: VersionedHeader,
    pub event_type: u32,
    pub value_length: u32,
    pub value_utf8: *const c_char,
    pub height_css_px: f64,
}

// SAFETY: all fields are zero-valid and v1 requires the complete record.
unsafe impl AbiStruct for MigoKeyboardEvent {}

#[derive(Clone, Debug, PartialEq)]
pub enum ValidatedKeyboardEvent {
    Input(String),
    Confirm(String),
    Complete(String),
    HeightChange(f64),
}

impl MigoKeyboardEvent {
    /// # Safety
    /// `event` must be readable for its announced size. For a text variant,
    /// `value_utf8` must reference value_length readable bytes unless zero.
    pub unsafe fn parse(event: *const Self) -> Result<ValidatedKeyboardEvent, MigoResult> {
        // SAFETY: forwarded from this method's contract.
        let raw = unsafe { copy_versioned::<Self>(event.cast()) }?;
        unsafe { raw.validate_fields() }
    }

    /// # Safety
    /// For a text variant, `value_utf8` must reference value_length readable
    /// bytes unless the length is zero.
    pub unsafe fn validate(&self) -> Result<ValidatedKeyboardEvent, MigoResult> {
        // SAFETY: Self is complete and the caller supplies the pointee contract.
        let raw = unsafe { copy_versioned::<Self>((self as *const Self).cast()) }?;
        unsafe { raw.validate_fields() }
    }

    unsafe fn validate_fields(self) -> Result<ValidatedKeyboardEvent, MigoResult> {
        // Select the union arm before inspecting fields irrelevant to it.
        match self.event_type {
            MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE => {
                let height = validate_f64_range(self.height_css_px, 0.0..=f64::MAX)?;
                Ok(ValidatedKeyboardEvent::HeightChange(height))
            }
            MIGO_KEYBOARD_EVENT_INPUT
            | MIGO_KEYBOARD_EVENT_CONFIRM
            | MIGO_KEYBOARD_EVENT_COMPLETE => {
                // SAFETY: forwarded from the public method contract.
                let value = unsafe { copy_utf8_with_length(self.value_utf8, self.value_length) }?;
                Ok(match self.event_type {
                    MIGO_KEYBOARD_EVENT_INPUT => ValidatedKeyboardEvent::Input(value),
                    MIGO_KEYBOARD_EVENT_CONFIRM => ValidatedKeyboardEvent::Confirm(value),
                    MIGO_KEYBOARD_EVENT_COMPLETE => ValidatedKeyboardEvent::Complete(value),
                    _ => unreachable!(),
                })
            }
            _ => Err(MIGO_ERROR_INVALID_ARGUMENT),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoKeyEvent {
    pub header: VersionedHeader,
    pub event_type: u32,
    pub key_length: u32,
    pub key_utf8: *const c_char,
    pub code_utf8: *const c_char,
    pub code_length: u32,
    pub reserved0: u32,
    pub timestamp_ms: f64,
    pub modifiers: u32,
    pub flags: u32,
}

// SAFETY: all fields are zero-valid, and the appended tail is optional: a host
// built before modifiers existed announces the shorter size, and zero is
// exactly what it means -- no modifier held, not an auto-repeat.
unsafe impl AbiStruct for MigoKeyEvent {
    // Expressed as an offset rather than a literal so it stays correct on every
    // pointer width; the record holds two pointers, so the number differs
    // between LP64 and ILP32 and a literal would be right for only one.
    const MINIMUM_SIZE: usize = offset_of!(MigoKeyEvent, modifiers);
}

#[derive(Clone, Debug, PartialEq)]
pub enum ValidatedKeyEvent {
    Down {
        key: String,
        code: String,
        timestamp_ms: f64,
        modifiers: u32,
        repeat: bool,
    },
    Up {
        key: String,
        code: String,
        timestamp_ms: f64,
        modifiers: u32,
        repeat: bool,
    },
}

impl MigoKeyEvent {
    /// # Safety
    /// `event` and both announced string ranges must be readable for this call.
    pub unsafe fn parse(event: *const Self) -> Result<ValidatedKeyEvent, MigoResult> {
        let raw = unsafe { copy_versioned::<Self>(event.cast()) }?;
        unsafe { raw.validate_fields() }
    }

    /// # Safety
    /// Both announced string ranges must be readable for this call.
    pub unsafe fn validate(&self) -> Result<ValidatedKeyEvent, MigoResult> {
        let raw = unsafe { copy_versioned::<Self>((self as *const Self).cast()) }?;
        unsafe { raw.validate_fields() }
    }

    unsafe fn validate_fields(self) -> Result<ValidatedKeyEvent, MigoResult> {
        match self.event_type {
            MIGO_KEY_EVENT_DOWN | MIGO_KEY_EVENT_UP => {}
            _ => return Err(MIGO_ERROR_INVALID_ARGUMENT),
        }
        validate_reserved(self.reserved0.into())?;
        validate_flags(self.modifiers.into(), MIGO_KEY_MODIFIER_KNOWN_MASK.into())?;
        validate_flags(self.flags.into(), MIGO_KEY_EVENT_FLAG_KNOWN_MASK.into())?;
        if !self.timestamp_ms.is_finite() {
            return Err(MIGO_ERROR_INVALID_ARGUMENT);
        }
        let key = unsafe { copy_utf8_with_length(self.key_utf8, self.key_length) }?;
        let code = unsafe { copy_utf8_with_length(self.code_utf8, self.code_length) }?;
        if code.is_empty() {
            return Err(MIGO_ERROR_INVALID_ARGUMENT);
        }
        let modifiers = self.modifiers;
        let repeat = self.flags & MIGO_KEY_EVENT_FLAG_REPEAT != 0;
        Ok(if self.event_type == MIGO_KEY_EVENT_DOWN {
            ValidatedKeyEvent::Down {
                key,
                code,
                timestamp_ms: self.timestamp_ms,
                modifiers,
                repeat,
            }
        } else {
            ValidatedKeyEvent::Up {
                key,
                code,
                timestamp_ms: self.timestamp_ms,
                modifiers,
                repeat,
            }
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoCompositionEvent {
    pub header: VersionedHeader,
    pub event_type: u32,
    pub data_length: u32,
    pub data_utf8: *const c_char,
}

// SAFETY: all fields are zero-valid and v1 requires the complete record.
unsafe impl AbiStruct for MigoCompositionEvent {}

#[derive(Clone, Debug, PartialEq)]
pub enum ValidatedCompositionEvent {
    Start(String),
    Update(String),
    End(String),
}

impl MigoCompositionEvent {
    /// # Safety
    /// `event` and its announced string range must be readable for this call.
    pub unsafe fn parse(event: *const Self) -> Result<ValidatedCompositionEvent, MigoResult> {
        let raw = unsafe { copy_versioned::<Self>(event.cast()) }?;
        unsafe { raw.validate_fields() }
    }

    /// # Safety
    /// The announced string range must be readable for this call.
    pub unsafe fn validate(&self) -> Result<ValidatedCompositionEvent, MigoResult> {
        let raw = unsafe { copy_versioned::<Self>((self as *const Self).cast()) }?;
        unsafe { raw.validate_fields() }
    }

    unsafe fn validate_fields(self) -> Result<ValidatedCompositionEvent, MigoResult> {
        match self.event_type {
            MIGO_COMPOSITION_EVENT_START
            | MIGO_COMPOSITION_EVENT_UPDATE
            | MIGO_COMPOSITION_EVENT_END => {}
            _ => return Err(MIGO_ERROR_INVALID_ARGUMENT),
        }
        let data = unsafe { copy_utf8_with_length(self.data_utf8, self.data_length) }?;
        Ok(match self.event_type {
            MIGO_COMPOSITION_EVENT_START => ValidatedCompositionEvent::Start(data),
            MIGO_COMPOSITION_EVENT_UPDATE => ValidatedCompositionEvent::Update(data),
            MIGO_COMPOSITION_EVENT_END => ValidatedCompositionEvent::End(data),
            _ => unreachable!(),
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MigoGamepadButton {
    pub flags: u32,
    pub value: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoGamepadInfo {
    pub header: VersionedHeader,
    pub index: u32,
    pub axis_count: u32,
    pub button_count: u32,
    pub reserved0: u32,
    pub id_utf8: *const c_char,
    pub mapping_utf8: *const c_char,
}

// SAFETY: all fields are zero-valid and v1 requires the complete record.
unsafe impl AbiStruct for MigoGamepadInfo {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidatedGamepadConnection {
    Connected {
        index: u32,
        axis_count: u8,
        button_count: u8,
        id: String,
        mapping: String,
    },
    Disconnected {
        index: u32,
    },
}

impl MigoGamepadInfo {
    /// # Safety
    /// `info` must be readable for its announced size. On connect, both string
    /// pointers must reference readable NUL-terminated strings.
    pub unsafe fn parse_connection(
        info: *const Self,
        connected: u32,
    ) -> Result<ValidatedGamepadConnection, MigoResult> {
        let raw = unsafe { copy_versioned::<Self>(info.cast()) }?;
        unsafe { raw.validate_connection_fields(connected) }
    }

    /// # Safety
    /// On connect, both pointers must reference readable NUL-terminated strings.
    pub unsafe fn validate_connection(
        &self,
        connected: u32,
    ) -> Result<ValidatedGamepadConnection, MigoResult> {
        let raw = unsafe { copy_versioned::<Self>((self as *const Self).cast()) }?;
        unsafe { raw.validate_connection_fields(connected) }
    }

    unsafe fn validate_connection_fields(
        self,
        connected: u32,
    ) -> Result<ValidatedGamepadConnection, MigoResult> {
        validate_reserved(self.reserved0.into())?;
        if self.index as usize >= MIGO_GAMEPAD_MAX_COUNT {
            return Err(MIGO_ERROR_INVALID_ARGUMENT);
        }
        match connected {
            0 => Ok(ValidatedGamepadConnection::Disconnected { index: self.index }),
            1 => {
                if self.axis_count as usize > MIGO_GAMEPAD_MAX_AXES
                    || self.button_count as usize > MIGO_GAMEPAD_MAX_BUTTONS
                {
                    return Err(MIGO_ERROR_INVALID_ARGUMENT);
                }
                let id = unsafe { copy_utf8(self.id_utf8) }?;
                let mapping = unsafe { copy_utf8(self.mapping_utf8) }?;
                if !matches!(mapping.as_str(), "" | "standard") {
                    return Err(MIGO_ERROR_INVALID_ARGUMENT);
                }
                Ok(ValidatedGamepadConnection::Connected {
                    index: self.index,
                    axis_count: self.axis_count as u8,
                    button_count: self.button_count as u8,
                    id,
                    mapping,
                })
            }
            _ => Err(MIGO_ERROR_INVALID_ARGUMENT),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoGamepadStateEvent {
    pub header: VersionedHeader,
    pub index: u32,
    pub axis_count: u32,
    pub button_count: u32,
    pub reserved0: u32,
    pub axes: *const f32,
    pub buttons: *const MigoGamepadButton,
    pub timestamp_ms: f64,
}

// SAFETY: all fields are zero-valid and v1 requires the complete record.
unsafe impl AbiStruct for MigoGamepadStateEvent {}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ValidatedGamepadButton {
    pressed: bool,
    touched: bool,
    value: f32,
}

impl ValidatedGamepadButton {
    #[inline]
    pub const fn pressed(self) -> bool {
        self.pressed
    }

    #[inline]
    pub const fn touched(self) -> bool {
        self.touched
    }

    #[inline]
    pub const fn value(self) -> f32 {
        self.value
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValidatedGamepadState {
    index: u32,
    axis_count: u8,
    button_count: u8,
    axes: [f32; MIGO_GAMEPAD_MAX_AXES],
    buttons: [ValidatedGamepadButton; MIGO_GAMEPAD_MAX_BUTTONS],
    timestamp_ms: f64,
}

impl ValidatedGamepadState {
    #[inline]
    pub const fn index(&self) -> u32 {
        self.index
    }

    #[inline]
    pub fn axes(&self) -> &[f32] {
        &self.axes[..self.axis_count as usize]
    }

    #[inline]
    pub fn buttons(&self) -> &[ValidatedGamepadButton] {
        &self.buttons[..self.button_count as usize]
    }

    #[inline]
    pub const fn timestamp_ms(&self) -> f64 {
        self.timestamp_ms
    }

    #[inline]
    pub const fn into_parts(
        self,
    ) -> (
        u32,
        u8,
        u8,
        [f32; MIGO_GAMEPAD_MAX_AXES],
        [ValidatedGamepadButton; MIGO_GAMEPAD_MAX_BUTTONS],
        f64,
    ) {
        (
            self.index,
            self.axis_count,
            self.button_count,
            self.axes,
            self.buttons,
            self.timestamp_ms,
        )
    }
}

impl MigoGamepadStateEvent {
    /// # Safety
    /// `event` must be readable for its announced size; non-zero counts require
    /// matching arrays of readable elements.
    pub unsafe fn parse(event: *const Self) -> Result<ValidatedGamepadState, MigoResult> {
        let raw = unsafe { copy_versioned::<Self>(event.cast()) }?;
        unsafe { raw.validate_fields() }
    }

    /// # Safety
    /// Non-zero counts require matching arrays of readable elements.
    pub unsafe fn validate(&self) -> Result<ValidatedGamepadState, MigoResult> {
        let raw = unsafe { copy_versioned::<Self>((self as *const Self).cast()) }?;
        unsafe { raw.validate_fields() }
    }

    unsafe fn validate_fields(self) -> Result<ValidatedGamepadState, MigoResult> {
        validate_reserved(self.reserved0.into())?;
        if self.index as usize >= MIGO_GAMEPAD_MAX_COUNT
            || self.axis_count as usize > MIGO_GAMEPAD_MAX_AXES
            || self.button_count as usize > MIGO_GAMEPAD_MAX_BUTTONS
            || (self.axis_count != 0 && self.axes.is_null())
            || (self.button_count != 0 && self.buttons.is_null())
        {
            return Err(MIGO_ERROR_INVALID_ARGUMENT);
        }
        let timestamp_ms = validate_f64_range(self.timestamp_ms, 0.0..=f64::MAX)?;

        let axis_count = self.axis_count as usize;
        let mut axes = [0.0; MIGO_GAMEPAD_MAX_AXES];
        if axis_count != 0 {
            let source = unsafe { std::slice::from_raw_parts(self.axes, axis_count) };
            for &axis in source {
                validate_f32_range(axis, -1.0..=1.0)?;
            }
            axes[..axis_count].copy_from_slice(source);
        }

        let button_count = self.button_count as usize;
        let mut buttons = [ValidatedGamepadButton::default(); MIGO_GAMEPAD_MAX_BUTTONS];
        if button_count != 0 {
            let source = unsafe { std::slice::from_raw_parts(self.buttons, button_count) };
            for (destination, raw) in buttons[..button_count].iter_mut().zip(source) {
                validate_flags(raw.flags.into(), MIGO_GAMEPAD_BUTTON_FLAG_KNOWN_MASK.into())?;
                let value = validate_f32_range(raw.value, 0.0..=1.0)?;
                *destination = ValidatedGamepadButton {
                    pressed: raw.flags & MIGO_GAMEPAD_BUTTON_FLAG_PRESSED != 0,
                    touched: raw.flags & MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED != 0,
                    value,
                };
            }
        }

        Ok(ValidatedGamepadState {
            index: self.index,
            axis_count: axis_count as u8,
            button_count: button_count as u8,
            axes,
            buttons,
            timestamp_ms,
        })
    }
}

const _: () = assert!(size_of::<MigoTouchPoint>() == 20);
const _: () = assert!(offset_of!(MigoTouchPoint, id) == 0);
const _: () = assert!(offset_of!(MigoTouchPoint, x) == 4);
const _: () = assert!(offset_of!(MigoTouchPoint, y) == 8);
const _: () = assert!(offset_of!(MigoTouchPoint, pressure) == 12);
const _: () = assert!(offset_of!(MigoTouchPoint, flags) == 16);
const _: () = assert!(offset_of!(MigoTouchEvent, header) == 0);
const _: () = assert!(offset_of!(MigoKeyboardEvent, header) == 0);
const _: () = assert!(offset_of!(MigoKeyEvent, header) == 0);
const _: () = assert!(offset_of!(MigoCompositionEvent, header) == 0);
// Pointer-free, so these are asserted unconditionally rather than under a
// pointer-width cfg: one layout serves LP64, LLP64 and ILP32, and putting them
// behind a cfg would leave the other widths' halves uncompiled -- exactly the
// dead-branch mistake `MigoHostCallbacks`'s ILP32 size hid for two appends.
const _: () = assert!(size_of::<MigoPointerEvent>() == 32);
const _: () = assert!(offset_of!(MigoPointerEvent, header) == 0);
const _: () = assert!(offset_of!(MigoPointerEvent, event_type) == 8);
const _: () = assert!(offset_of!(MigoPointerEvent, button) == 12);
const _: () = assert!(offset_of!(MigoPointerEvent, x) == 16);
const _: () = assert!(offset_of!(MigoPointerEvent, y) == 20);
const _: () = assert!(offset_of!(MigoPointerEvent, timestamp_ms) == 24);
const _: () = assert!(size_of::<MigoWheelEvent>() == 48);
const _: () = assert!(offset_of!(MigoWheelEvent, header) == 0);
const _: () = assert!(offset_of!(MigoWheelEvent, delta_mode) == 8);
const _: () = assert!(offset_of!(MigoWheelEvent, reserved0) == 12);
const _: () = assert!(offset_of!(MigoWheelEvent, delta_x) == 16);
const _: () = assert!(offset_of!(MigoWheelEvent, delta_y) == 24);
const _: () = assert!(offset_of!(MigoWheelEvent, delta_z) == 32);
const _: () = assert!(offset_of!(MigoWheelEvent, timestamp_ms) == 40);

const _: () = assert!(size_of::<MigoGamepadButton>() == 8);
const _: () = assert!(offset_of!(MigoGamepadButton, value) == 4);
const _: () = assert!(offset_of!(MigoGamepadInfo, header) == 0);
const _: () = assert!(offset_of!(MigoGamepadStateEvent, header) == 0);

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoTouchEvent>() == 32);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoTouchEvent, points) == 24);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoKeyboardEvent>() == 32);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoKeyboardEvent, value_utf8) == 16);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoKeyEvent>() == 56);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoKeyEvent, timestamp_ms) == 40);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoKeyEvent, modifiers) == 48);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoKeyEvent, flags) == 52);
// The appended tail must stay optional: an old host announces exactly this many
// bytes, and a MINIMUM_SIZE that crept up to the full size would reject it.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(<MigoKeyEvent as AbiStruct>::MINIMUM_SIZE == 48);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoCompositionEvent>() == 24);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoGamepadInfo>() == 40);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoGamepadInfo, id_utf8) == 24);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoGamepadStateEvent>() == 48);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoGamepadStateEvent, axes) == 24);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoGamepadStateEvent, buttons) == 32);
