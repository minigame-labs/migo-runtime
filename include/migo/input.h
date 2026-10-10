/*
 * Migo C ABI: input.
 *
 * Touch, the desktop pointer (mouse buttons, motion, wheel), soft keyboard,
 * physical keys, IME composition and gamepads are all carried here. See
 * README.md for the state of each contract against the v1 freeze.
 */
#ifndef MIGO_INPUT_H
#define MIGO_INPUT_H

#include <stddef.h> /* offsetof */
#include <migo/types.h>
#include <migo/session.h>

MIGO_BEGIN_DECLS

/*
 * Input acceptance and backpressure.
 *
 * Every migo_session_send_* input call is nonblocking. MIGO_OK means the event
 * was either enqueued or safely coalesced with an older pending state from the
 * same stream. MIGO_ERROR_WOULD_BLOCK means the event was not accepted; the
 * host may retry while it is still current.
 *
 * High-rate motion/state samples are coalesced. State transitions use a
 * separate reliable reserve so they remain deliverable when normal input is
 * full, but that reserve is intentionally bounded: a producer that ignores
 * backpressure can exhaust it. The first refusal in a saturation episode is
 * also reported through MigoOnErrorFn; a later successful input call rearms
 * that notification.
 *
 * Backpressure is only reached once a call is well-formed. Every
 * migo_session_send_* call validates its Session and its event record the same
 * way first, so these three rejections are possible for all of them and are
 * not repeated in the per-function notes below:
 *
 *   MIGO_ERROR_INVALID_ARGUMENT  session or the event pointer was NULL, the
 *                                event's struct_size is smaller than the
 *                                minimum versioned record, or a field holds a
 *                                value this ABI does not define
 *   MIGO_ERROR_UNSUPPORTED_ABI   the event's abi_version does not match this
 *                                engine build, or struct_size claims a record
 *                                larger than this build knows
 *   MIGO_ERROR_INVALID_STATE     the Session has already been destroyed, or it
 *                                has no live host to deliver to
 *
 * A rejected event is never partially delivered: validation completes before
 * anything is enqueued, so a failed call leaves the input stream untouched.
 */

/*
 * Timestamps.
 *
 * Every event carries `timestamp_ms` from the host's own clock, and this ABI
 * names none: use the one the platform stamps its input with (the system uptime
 * on Apple and Android is what their events already carry). What the engine
 * relies on is only that one host's stamps are on ONE clock whose differences
 * are real time. The engine anchors that clock to the page's at the first event
 * of each burst of input and gives content `event.timeStamp` on the clock
 * `performance.now()` reads, with the host's spacing inside the burst, so
 * nothing about the host's epoch reaches the game. Do not convert a stamp
 * yourself, and do not mix two clocks in one stream.
 */

/*
 * Touch, including on desktop. Mini-game content listens for
 * touchstart/touchmove/touchend and nothing else, so a host with a mouse maps it
 * to a single touch point with id 0 rather than expecting a separate pointer
 * event model.
 */
typedef uint32_t MigoTouchType;
#define MIGO_TOUCH_START 0U
#define MIGO_TOUCH_MOVE 1U
#define MIGO_TOUCH_END 2U
#define MIGO_TOUCH_CANCEL 3U

typedef uint32_t MigoTouchPointFlags;
#define MIGO_TOUCH_FLAG_NONE 0U
/* This point is part of the event's changedTouches list. */
#define MIGO_TOUCH_FLAG_CHANGED (1U << 0)
/* This point left the surface with this event, so it is excluded from touches. */
#define MIGO_TOUCH_FLAG_REMOVED (1U << 1)

/* Bounded by the engine's fixed inline array; larger counts are rejected. */
#define MIGO_TOUCH_MAX_POINTS 10

/*
 * One pointer within an event.
 *
 * x and y are CSS pixels -- logical coordinates, not physical ones. A host
 * converts from its own pixels using the scale_factor it supplied at attach.
 * Sending physical pixels is the single most likely integration mistake: the
 * game renders correctly and input lands somewhere else.
 *
 * No struct_size: this is an array element, and a per-point size prefix would
 * break the single-copy delivery path. The layout is pinned by static assertion
 * on both sides of the boundary instead.
 */
typedef struct MigoTouchPoint {
    uint32_t id; /* stable across start -> move -> end for one pointer */
    float x;
    float y;
    /* [0.0, 1.0], the same contract as the web's Touch.force; 0.0 when the
     * device does not report pressure. Most touch platforms calibrate
     * pressure per device rather than normalizing it -- Android's own docs
     * say values above 1.0 are expected -- so a host reading a native
     * pressure API almost always has to clamp before crossing this boundary,
     * the same way it converts physical pixels to CSS pixels for x and y. An
     * out-of-range value here is rejected as MIGO_ERROR_INVALID_ARGUMENT. */
    float pressure;
    MigoTouchPointFlags flags;
} MigoTouchPoint;

MIGO_STATIC_ASSERT(sizeof(MigoTouchPoint) == 20, "MigoTouchPoint is 20 bytes on every target");
MIGO_STATIC_ASSERT(offsetof(MigoTouchPoint, id) == 0, "MigoTouchPoint.id moved");
MIGO_STATIC_ASSERT(offsetof(MigoTouchPoint, x) == 4, "MigoTouchPoint.x moved");
MIGO_STATIC_ASSERT(offsetof(MigoTouchPoint, y) == 8, "MigoTouchPoint.y moved");
MIGO_STATIC_ASSERT(offsetof(MigoTouchPoint, pressure) == 12, "MigoTouchPoint.pressure moved");
MIGO_STATIC_ASSERT(offsetof(MigoTouchPoint, flags) == 16, "MigoTouchPoint.flags moved");

/*
 * points is borrowed for the duration of the call: the implementation copies
 * what it needs before returning, so the caller may reuse or free the array
 * immediately afterwards.
 */
typedef struct MigoTouchEvent {
    uint32_t struct_size;
    uint32_t abi_version;
    MigoTouchType type;
    uint32_t point_count; /* 1..MIGO_TOUCH_MAX_POINTS */
    int64_t timestamp_ms;
    const MigoTouchPoint *points;
} MigoTouchEvent;

MIGO_STATIC_ASSERT(offsetof(MigoTouchEvent, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
#if MIGO_LP64
MIGO_STATIC_ASSERT(sizeof(MigoTouchEvent) == 32, "MigoTouchEvent LP64 size changed");
MIGO_STATIC_ASSERT(offsetof(MigoTouchEvent, timestamp_ms) == 16, "MigoTouchEvent.timestamp_ms moved");
MIGO_STATIC_ASSERT(offsetof(MigoTouchEvent, points) == 24, "MigoTouchEvent.points moved");
#endif

/*
 * Deliver one touch event to the session's content.
 *
 * Safe to call from any thread, but events are delivered in call order, so a
 * host calling concurrently from two threads gets undefined ordering. Every
 * windowing system already serialises its event loop; keep the input stream on
 * one thread.
 *
 * Returns MIGO_ERROR_INVALID_STATE when no surface is attached -- there is
 * nothing to deliver to -- and MIGO_ERROR_WOULD_BLOCK when the event was not
 * accepted.
 */
MIGO_API MigoResult MIGO_CALL migo_session_send_touch(
    MigoSession *session,
    const MigoTouchEvent *event);

/*
 * Desktop pointer: mouse buttons, motion, and the wheel.
 *
 * Separate from touch above, and a host chooses which streams it sends.
 * Mini-game content written for a phone listens for touch; content written
 * for a PC mini-game platform listens for the mouse, which is why these
 * names are on the mini-game surface at all. A desktop host serving
 * phone-first content may send both. Migo
 * synthesizes neither from the other: only the host knows what its content and
 * its device call for, and a runtime that guessed would double-deliver every
 * click to content that listens for both.
 *
 * Neither record carries a pointer, so both have one layout on every target --
 * LP64, LLP64 and ILP32 agree, unlike the string-bearing records below.
 */
typedef uint32_t MigoPointerEventType;
#define MIGO_POINTER_EVENT_DOWN 0U
#define MIGO_POINTER_EVENT_MOVE 1U
#define MIGO_POINTER_EVENT_UP 2U

/* Highest accepted button ordinal; the DOM defines five. */
#define MIGO_POINTER_BUTTON_MAX 31U

/*
 * x and y are CSS pixels, exactly as in MigoTouchPoint -- the same conversion
 * from the host's own pixels using the scale_factor supplied at attach. Sending
 * physical pixels here has the same symptom it has for touch: the game renders
 * correctly and the click lands somewhere else. Sending touch in one unit and
 * the pointer in the other is worse, because one press then reports two
 * different positions.
 *
 * button follows DOM MouseEvent.button: 0 primary, 1 auxiliary, 2 secondary.
 * On a move it names the button being held.
 */
typedef struct MigoPointerEvent {
    uint32_t struct_size;
    uint32_t abi_version;
    MigoPointerEventType event_type;
    uint32_t button;
    float x;
    float y;
    double timestamp_ms;
} MigoPointerEvent;

MIGO_STATIC_ASSERT(offsetof(MigoPointerEvent, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
MIGO_STATIC_ASSERT(sizeof(MigoPointerEvent) == 32, "MigoPointerEvent size changed");
MIGO_STATIC_ASSERT(offsetof(MigoPointerEvent, event_type) == 8, "MigoPointerEvent.event_type moved");
MIGO_STATIC_ASSERT(offsetof(MigoPointerEvent, button) == 12, "MigoPointerEvent.button moved");
MIGO_STATIC_ASSERT(offsetof(MigoPointerEvent, x) == 16, "MigoPointerEvent.x moved");
MIGO_STATIC_ASSERT(offsetof(MigoPointerEvent, y) == 20, "MigoPointerEvent.y moved");
MIGO_STATIC_ASSERT(offsetof(MigoPointerEvent, timestamp_ms) == 24,
                   "MigoPointerEvent.timestamp_ms moved");

/*
 * Deliver one mouse button or motion event. Callable from any thread; ordering
 * between concurrent calls is the host's to guarantee.
 *
 * Returns MIGO_ERROR_INVALID_STATE when no surface is attached and
 * MIGO_ERROR_WOULD_BLOCK when the event was not accepted. DOWN and UP use the
 * reliable reserve; MOVE is coalescible.
 */
MIGO_API MigoResult MIGO_CALL migo_session_send_pointer_event(
    MigoSession *session,
    const MigoPointerEvent *event);

/* DOM WheelEvent.deltaMode: what unit the deltas are in. */
typedef uint32_t MigoWheelDeltaMode;
#define MIGO_WHEEL_DELTA_MODE_PIXEL 0U
#define MIGO_WHEEL_DELTA_MODE_LINE 1U
#define MIGO_WHEEL_DELTA_MODE_PAGE 2U

/*
 * One scroll. The deltas follow DOM WheelEvent, and delta_mode travels with
 * them rather than being normalized to pixels by the host, because converting a
 * line- or page-based delta needs the content's own line height. A toolkit that
 * reports scroll in degrees (Qt's angleDelta is eighths of a degree) converts to
 * one of these units; it must not report degrees as pixels.
 *
 * An unrecognised delta_mode is rejected rather than assumed to be pixels: a
 * line-sized number read as pixels scrolls by a fraction of what the user asked
 * for, and nothing later corrects it.
 */
typedef struct MigoWheelEvent {
    uint32_t struct_size;
    uint32_t abi_version;
    MigoWheelDeltaMode delta_mode;
    uint32_t reserved0;
    double delta_x;
    double delta_y;
    double delta_z; /* 0.0 on the devices that do not report depth */
    double timestamp_ms;
} MigoWheelEvent;

MIGO_STATIC_ASSERT(offsetof(MigoWheelEvent, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
MIGO_STATIC_ASSERT(sizeof(MigoWheelEvent) == 48, "MigoWheelEvent size changed");
MIGO_STATIC_ASSERT(offsetof(MigoWheelEvent, delta_mode) == 8, "MigoWheelEvent.delta_mode moved");
MIGO_STATIC_ASSERT(offsetof(MigoWheelEvent, reserved0) == 12, "MigoWheelEvent.reserved0 moved");
MIGO_STATIC_ASSERT(offsetof(MigoWheelEvent, delta_x) == 16, "MigoWheelEvent.delta_x moved");
MIGO_STATIC_ASSERT(offsetof(MigoWheelEvent, delta_y) == 24, "MigoWheelEvent.delta_y moved");
MIGO_STATIC_ASSERT(offsetof(MigoWheelEvent, delta_z) == 32, "MigoWheelEvent.delta_z moved");
MIGO_STATIC_ASSERT(offsetof(MigoWheelEvent, timestamp_ms) == 40,
                   "MigoWheelEvent.timestamp_ms moved");

/*
 * Deliver one wheel event. Callable from any thread; ordering between
 * concurrent calls is the host's to guarantee.
 *
 * Returns MIGO_ERROR_INVALID_STATE when no surface is attached -- there is
 * nothing to deliver to -- and MIGO_ERROR_WOULD_BLOCK when the event was not
 * accepted.
 */
MIGO_API MigoResult MIGO_CALL migo_session_send_wheel_event(
    MigoSession *session,
    const MigoWheelEvent *event);

/*
 * Soft keyboard, common mini-game platform model.
 *
 * The value a text event carries is the field's WHOLE CURRENT TEXT, not the
 * keystroke that changed it. A host that sends only the newly typed character
 * leaves content whose text never grows past one character. In the other
 * direction, MigoOnUpdateKeyboardFn is content correcting that same value.
 */
typedef uint32_t MigoKeyboardEventType;
#define MIGO_KEYBOARD_EVENT_INPUT 0U
#define MIGO_KEYBOARD_EVENT_CONFIRM 1U
#define MIGO_KEYBOARD_EVENT_COMPLETE 2U
#define MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE 3U

/*
 * value_utf8 is length-delimited and need not be NUL-terminated; only
 * value_length bytes are read, and it is borrowed for the duration of the call.
 * A zero length with a non-null pointer is valid: it is the field being
 * cleared.
 *
 * height_css_px applies to MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE only and is CSS
 * pixels -- logical, not physical, the same units as touch coordinates. Zero
 * means the keyboard is gone. Sending physical pixels lays content out for a
 * keyboard of the wrong size on every display whose scale factor is not 1.
 */
typedef struct MigoKeyboardEvent {
    uint32_t struct_size;
    uint32_t abi_version;
    MigoKeyboardEventType event_type;
    uint32_t value_length;
    const char *value_utf8;
    double height_css_px;
} MigoKeyboardEvent;

MIGO_STATIC_ASSERT(offsetof(MigoKeyboardEvent, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
#if MIGO_LP64
MIGO_STATIC_ASSERT(sizeof(MigoKeyboardEvent) == 32, "MigoKeyboardEvent LP64 size changed");
MIGO_STATIC_ASSERT(offsetof(MigoKeyboardEvent, value_utf8) == 16,
                   "MigoKeyboardEvent.value_utf8 moved");
MIGO_STATIC_ASSERT(offsetof(MigoKeyboardEvent, height_css_px) == 24,
                   "MigoKeyboardEvent.height_css_px moved");
#endif

/*
 * Deliver one soft-keyboard event. Callable from any thread; ordering between
 * concurrent calls is the host's to guarantee.
 *
 * Returns MIGO_ERROR_INVALID_STATE when no surface is attached and
 * MIGO_ERROR_WOULD_BLOCK when the event was not accepted. COMPLETE uses the
 * reliable reserve.
 */
MIGO_API MigoResult MIGO_CALL migo_session_send_keyboard_event(
    MigoSession *session,
    const MigoKeyboardEvent *event);

/*
 * Physical keys.
 *
 * A different capability from the soft keyboard above, despite the shared word.
 * The soft keyboard is text: the host owns an IME and reports the field's whole
 * current value. This is a discrete press of an identified key, and content
 * reads the two through different listeners.
 *
 * Not batched, unlike touch: keys arrive at human typing speed, one at a time,
 * so a batch API would be shape without a requirement.
 */
typedef uint32_t MigoKeyEventType;
#define MIGO_KEY_EVENT_DOWN 0U
#define MIGO_KEY_EVENT_UP 1U

/*
 * key and code are DOM values, and they are NOT interchangeable:
 *
 *   code  identifies the physical key   -- "KeyA", "ArrowLeft", "Escape"
 *   key   is what it produces, given the current modifiers and layout
 *                                       -- "a", "A", "ArrowLeft"
 *
 * A host has platform keycodes -- AKEYCODE_A, an X11 keysym -- and translating
 * them is the host's work. That table lives here rather than in Migo because a
 * portable runtime that accepted platform codes would have to carry a mapping
 * per platform. Sending code in both fields is the likely mistake, and it
 * produces content that reads "KeyA" as typed text.
 *
 * An empty key is legitimate: a dead key produces no text. An empty code is
 * rejected, because a code always identifies something.
 *
 * Both strings are length-delimited, need not be NUL-terminated, and are
 * borrowed for the duration of the call.
 */
typedef uint32_t MigoKeyModifiers;
#define MIGO_KEY_MODIFIER_NONE 0U
#define MIGO_KEY_MODIFIER_CONTROL (1U << 0)
#define MIGO_KEY_MODIFIER_SHIFT (1U << 1)
#define MIGO_KEY_MODIFIER_ALT (1U << 2)
#define MIGO_KEY_MODIFIER_META (1U << 3)

typedef uint32_t MigoKeyEventFlags;
#define MIGO_KEY_EVENT_FLAG_NONE 0U
/* The platform's auto-repeat produced this press, not the user pressing again.
 * DOM KeyboardEvent.repeat. */
#define MIGO_KEY_EVENT_FLAG_REPEAT (1U << 0)

/*
 * modifiers and flags were appended after this record shipped, so they are the
 * optional tail: a host built against the earlier header announces the smaller
 * struct_size and its absent fields read as zero, which is exactly what they
 * mean -- no modifier held, not a repeat. reserved0 stays reserved.
 *
 * modifiers cannot be derived from key: a modified press still reports the
 * character it produces, so without this content cannot tell Ctrl+S from S.
 */
typedef struct MigoKeyEvent {
    uint32_t struct_size;
    uint32_t abi_version;
    MigoKeyEventType event_type;
    uint32_t key_length;
    const char *key_utf8;
    const char *code_utf8;
    uint32_t code_length;
    uint32_t reserved0;
    double timestamp_ms;
    MigoKeyModifiers modifiers;
    MigoKeyEventFlags flags;
} MigoKeyEvent;

MIGO_STATIC_ASSERT(offsetof(MigoKeyEvent, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
#if MIGO_LP64
MIGO_STATIC_ASSERT(sizeof(MigoKeyEvent) == 56, "MigoKeyEvent LP64 size changed");
MIGO_STATIC_ASSERT(offsetof(MigoKeyEvent, key_utf8) == 16, "MigoKeyEvent.key_utf8 moved");
MIGO_STATIC_ASSERT(offsetof(MigoKeyEvent, code_utf8) == 24, "MigoKeyEvent.code_utf8 moved");
MIGO_STATIC_ASSERT(offsetof(MigoKeyEvent, timestamp_ms) == 40,
                   "MigoKeyEvent.timestamp_ms moved");
MIGO_STATIC_ASSERT(offsetof(MigoKeyEvent, modifiers) == 48, "MigoKeyEvent.modifiers moved");
MIGO_STATIC_ASSERT(offsetof(MigoKeyEvent, flags) == 52, "MigoKeyEvent.flags moved");
#else
MIGO_STATIC_ASSERT(sizeof(MigoKeyEvent) == 48, "MigoKeyEvent ILP32 size changed");
MIGO_STATIC_ASSERT(offsetof(MigoKeyEvent, timestamp_ms) == 32,
                   "MigoKeyEvent.timestamp_ms moved");
MIGO_STATIC_ASSERT(offsetof(MigoKeyEvent, modifiers) == 40, "MigoKeyEvent.modifiers moved");
MIGO_STATIC_ASSERT(offsetof(MigoKeyEvent, flags) == 44, "MigoKeyEvent.flags moved");
#endif

/*
 * Deliver one key press or release. Callable from any thread; ordering between
 * concurrent calls is the host's to guarantee.
 *
 * Returns MIGO_ERROR_INVALID_STATE when no surface is attached -- there is
 * nothing to deliver to. MIGO_ERROR_WOULD_BLOCK means the event was not
 * accepted. Key UP uses the reliable reserve so it is not silently lost
 * behind ordinary input.
 */
MIGO_API MigoResult MIGO_CALL migo_session_send_key_event(
    MigoSession *session,
    const MigoKeyEvent *event);

/*
 * IME composition.
 *
 * Mainstream mini-game platforms have no composition API, so the reference is the DOM CompositionEvent.
 * Composition is the IN-PROGRESS state of IME input: typing pinyin shows a
 * preedit string before any of it is committed. It is distinct from the soft
 * keyboard above, which reports text that has ALREADY been committed.
 *
 * A game drawing its own text field needs both -- the preedit to show what is
 * being typed, and the committed value to store -- so a host that supports an
 * IME sends composition events alongside the keyboard ones, not instead of them.
 */
typedef uint32_t MigoCompositionEventType;
#define MIGO_COMPOSITION_EVENT_START 0U
#define MIGO_COMPOSITION_EVENT_UPDATE 1U
#define MIGO_COMPOSITION_EVENT_END 2U

/*
 * data is the WHOLE current preedit string on start and update, never a delta:
 * content that received only what changed could not reconstruct the rest. On
 * end it is the committed text, and empty means the user cancelled -- which
 * content must still see, so it can clear the preedit it has been drawing.
 *
 * Length-delimited, need not be NUL-terminated, borrowed for the call. A length
 * that splits a multi-byte character is rejected rather than delivered mangled.
 */
typedef struct MigoCompositionEvent {
    uint32_t struct_size;
    uint32_t abi_version;
    MigoCompositionEventType event_type;
    uint32_t data_length;
    const char *data_utf8;
} MigoCompositionEvent;

MIGO_STATIC_ASSERT(offsetof(MigoCompositionEvent, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
#if MIGO_LP64
MIGO_STATIC_ASSERT(sizeof(MigoCompositionEvent) == 24, "MigoCompositionEvent LP64 size changed");
MIGO_STATIC_ASSERT(offsetof(MigoCompositionEvent, data_utf8) == 16,
                   "MigoCompositionEvent.data_utf8 moved");
#endif

/*
 * Deliver one composition event.
 *
 * Returns MIGO_ERROR_INVALID_STATE when no surface is attached -- there is
 * nothing to deliver to. MIGO_ERROR_WOULD_BLOCK means the event was not
 * accepted. UPDATE is coalescible; START and END use the reliable reserve.
 */
MIGO_API MigoResult MIGO_CALL migo_session_send_composition_event(
    MigoSession *session,
    const MigoCompositionEvent *event);

/*
 * Gamepads.
 *
 * The common mini-game platform's gamepad API (getGamepads, onGamepadConnected,
 * onGamepadDisconnected) follows the browser standard, the W3C Gamepad API --
 * what content in a WebView reads through navigator.getGamepads() -- and this
 * is its shape.
 *
 * The Web API is POLLED, not evented: content reads whatever is current each
 * frame. So a host announces a pad once, then pushes samples as often as it
 * samples, and a dropped sample is harmless because the next one replaces it
 * wholesale.
 */

/* Bounded by the engine's fixed inline arrays; larger counts are rejected
 * rather than truncated, because a dropped button is one content is watching. */
#define MIGO_GAMEPAD_MAX_AXES 8
#define MIGO_GAMEPAD_MAX_BUTTONS 20

typedef uint32_t MigoGamepadButtonFlags;
#define MIGO_GAMEPAD_BUTTON_FLAG_NONE 0U
#define MIGO_GAMEPAD_BUTTON_FLAG_PRESSED (1U << 0)
#define MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED (1U << 1)

/*
 * One button. value is the analogue position -- 0.0 to 1.0 for a trigger, and
 * 0.0 or 1.0 for a digital button.
 *
 * PRESSED is carried rather than derived from value, and the two are genuinely
 * independent: a device picks its own press threshold, and a trigger resting at
 * 0.25 may or may not count as pressed depending on the pad. Deriving it here
 * would overrule the device.
 *
 * No struct_size: this is an array element, like MigoTouchPoint.
 */
typedef struct MigoGamepadButton {
    MigoGamepadButtonFlags flags;
    float value;
} MigoGamepadButton;

MIGO_STATIC_ASSERT(sizeof(MigoGamepadButton) == 8,
                   "MigoGamepadButton is 8 bytes on every target");
MIGO_STATIC_ASSERT(offsetof(MigoGamepadButton, value) == 4, "MigoGamepadButton.value moved");

/*
 * A gamepad's identity, supplied when it appears and again when it goes away.
 *
 * axis_count and button_count are given on connect rather than inferred from
 * the first sample, because getGamepads() must return correctly sized arrays
 * from the moment the gamepadconnected listener runs -- content commonly reads
 * buttons.length there to decide which layout it is looking at.
 *
 * id is the device's own name, which content shows to a player. mapping is
 * "standard" when the host has mapped the pad onto the standard layout and ""
 * when it has not; content reads it to decide whether the button order can be
 * trusted. Both are NUL-terminated and borrowed for the call.
 */
typedef struct MigoGamepadInfo {
    uint32_t struct_size;
    uint32_t abi_version;
    uint32_t index; /* the slot the pad holds for as long as it stays connected */
    uint32_t axis_count;
    uint32_t button_count;
    uint32_t reserved0;
    const char *id_utf8;
    const char *mapping_utf8;
} MigoGamepadInfo;

MIGO_STATIC_ASSERT(offsetof(MigoGamepadInfo, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
#if MIGO_LP64
MIGO_STATIC_ASSERT(sizeof(MigoGamepadInfo) == 40, "MigoGamepadInfo LP64 size changed");
MIGO_STATIC_ASSERT(offsetof(MigoGamepadInfo, id_utf8) == 24, "MigoGamepadInfo.id_utf8 moved");
#endif

/*
 * One sample. axes and buttons are borrowed for the duration of the call and
 * must each hold at least their announced count.
 */
typedef struct MigoGamepadStateEvent {
    uint32_t struct_size;
    uint32_t abi_version;
    uint32_t index;
    uint32_t axis_count;
    uint32_t button_count;
    uint32_t reserved0;
    const float *axes; /* each -1.0..1.0, in the standard mapping order */
    const MigoGamepadButton *buttons;
    double timestamp_ms;
} MigoGamepadStateEvent;

MIGO_STATIC_ASSERT(offsetof(MigoGamepadStateEvent, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
#if MIGO_LP64
MIGO_STATIC_ASSERT(sizeof(MigoGamepadStateEvent) == 48,
                   "MigoGamepadStateEvent LP64 size changed");
MIGO_STATIC_ASSERT(offsetof(MigoGamepadStateEvent, axes) == 24,
                   "MigoGamepadStateEvent.axes moved");
MIGO_STATIC_ASSERT(offsetof(MigoGamepadStateEvent, buttons) == 32,
                   "MigoGamepadStateEvent.buttons moved");
#endif

/*
 * Announce a gamepad, or withdraw it by passing 0 for connected. Withdrawing
 * reads only info->index, so a host may pass the same struct it connected with.
 *
 * An emptied slot stays empty rather than shifting the pads after it: content
 * holds on to an index.
 *
 * Returns MIGO_ERROR_INVALID_STATE when no surface is attached -- there is
 * nothing to announce to -- and MIGO_ERROR_WOULD_BLOCK when the event was not
 * accepted.
 */
MIGO_API MigoResult MIGO_CALL migo_session_set_gamepad_connected(
    MigoSession *session,
    const MigoGamepadInfo *info,
    uint32_t connected);

/*
 * Push one sample for a connected pad. Samples are coalesced independently by
 * gamepad index. MIGO_OK includes safe coalescing; MIGO_ERROR_WOULD_BLOCK means
 * this sample was not accepted. Returns MIGO_ERROR_INVALID_STATE when no
 * surface is attached -- there is nothing to deliver to.
 */
MIGO_API MigoResult MIGO_CALL migo_session_send_gamepad_state(
    MigoSession *session,
    const MigoGamepadStateEvent *event);

MIGO_END_DECLS

#endif /* MIGO_INPUT_H */
