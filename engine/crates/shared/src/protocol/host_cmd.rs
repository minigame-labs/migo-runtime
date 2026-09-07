//! # Host Command Protocol
//!
//! Defines commands sent to the host runtime thread from other engine components.
//! The host thread runs the JavaScript runtime and coordinates the game lifecycle.
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────┐     HostCommand      ┌──────────────┐
//! │ Render Thread├─────────────────────►│              │
//! └──────────────┘                      │              │
//! ┌──────────────┐     HostCommand      │  Host Thread │
//! │ Audio Thread ├─────────────────────►│  (JS Runtime)│
//! └──────────────┘                      │              │
//! ┌──────────────┐     HostCommand      │              │
//! │ Platform/JNI ├─────────────────────►│              │
//! └──────────────┘                      └──────────────┘
//! ```
//!
//! ## Command Categories
//!
//! The `HostCommand` enum is the authoritative list; the grouping and counts
//! below are indicative and may lag as variants (e.g. `SurfaceDestroyed`) are
//! added.
//!
//! - **Module Loading** (2): `EvaluateModule`, `EvalScript`
//! - **Lifecycle** (4): `Restart`, `Shutdown`, `OnShow`, `OnHide`
//! - **Audio** (3): `OnAudioInterruptionBegin`, `OnAudioInterruptionEnd`, `InnerAudioEvent`
//! - **Rendering / Surface** (1): `UpdateSurface`
//! - **Touch / Input** (1): `OnTouch`
//! - **Desktop pointer** (4): `OnMouseDown` .. `OnWheel`
//! - **Sensor Events** (5): `OnDeviceMotionChange` .. `OnAccelerometerChange`
//! - **Network** (1): `OnNetworkStatusChange`
//! - **Recorder** (2): `RecorderEvent`, `RecorderFrameData`
//! - **Camera** (2): `CameraEvent`, `CameraFrameData`
//! - **Keyboard** (6): `OnKeyboardInput` .. `OnKeyUp`
//! - **Gamepad** (3): `OnGamepadConnected` .. `OnGamepadState`
//! - **IME composition** (3): `OnCompositionStart` .. `OnCompositionEnd`
//! - **Bluetooth / BLE** (7): `OnBluetoothAdapterStateChange` .. `OnBeaconServiceChange`
//! - **Video** (1): `OnVideoStateChange`
//! - **System** (2): `OnMemoryWarning`, `OnUserCaptureScreen`

use std::borrow::Cow;
use std::num::NonZeroI64;

use crate::{
    payload_pool::{Pooled, Recycled},
    surface::{
        PixelRatio, PublicSurfaceGeneration, SurfaceCandidateRevision, SurfaceGeneration,
        SurfaceLease, SurfaceLossReason,
    },
};

/// Touch event payload, stored in a preallocated `HostCommand::OnTouch` slot.
///
/// Contains a fixed `[TouchPoint; 10]` array with a count field.
/// Single memcpy from JNI DirectByteBuffer into a bounded, preallocated
/// payload slot keeps steady-state input allocation-free.
#[derive(Debug)]
pub struct TouchData {
    /// Type of touch event (start, move, end, cancel).
    pub touch_type: TouchType,
    /// Number of valid touch points in the `points` array.
    pub count: u8,
    /// Fixed inline array of touch points (max 10 simultaneous).
    /// Only `points[..count]` are valid.
    pub points: [TouchPoint; 10],
    /// Event timestamp in milliseconds (from system boot or epoch).
    pub timestamp_ms: i64,
}

/// The largest gamepad this runtime carries state for.
///
/// The W3C standard mapping has 4 axes and 17 buttons; the headroom covers
/// devices that report a few more without making the payload variable-length.
/// The public ABI rejects a topology past these limits instead of silently
/// truncating buttons or axes that content may rely on; a platform adapter may
/// deliberately remap a device before it enters this transport.
pub const GAMEPAD_MAX_AXES: usize = 8;
pub const GAMEPAD_MAX_BUTTONS: usize = 20;

/// One button's state, matching the Web `GamepadButton`.
///
/// `value` is the analogue position for a trigger and 0.0/1.0 for a digital
/// button; `pressed` is not derivable from it, because a device chooses its own
/// press threshold and content must not have to guess one.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GamepadButtonState {
    pub pressed: bool,
    pub touched: bool,
    pub value: f32,
}

/// A gamepad's current state, boxed inside `HostCommand::OnGamepadState`.
///
/// Fixed inline arrays with counts, like `TouchData`: this arrives once per pad
/// per frame while a pad is connected, and a `Vec` here would allocate on the
/// input path for a payload whose maximum size is under 200 bytes.
#[derive(Debug)]
pub struct GamepadState {
    /// The slot this pad occupies, matching its index in `getGamepads()`.
    pub index: u32,
    /// Valid entries in `axes`.
    pub axis_count: u8,
    /// Valid entries in `buttons`.
    pub button_count: u8,
    /// Each in -1.0..=1.0, in the standard mapping's order.
    pub axes: [f32; GAMEPAD_MAX_AXES],
    pub buttons: [GamepadButtonState; GAMEPAD_MAX_BUTTONS],
    /// When the host sampled this state, in milliseconds.
    pub timestamp_ms: f64,
}

/// Retained capacity limit for one identifier field, in bytes.
///
/// A canonical GATT UUID is 36 characters and a Bluetooth device address is 17,
/// so nothing a conforming stack produces comes close. The limit exists for what
/// a non-conforming one could: an identifier is a string the platform hands us,
/// and one absurd value must not leave its buffer resident in a pooled slot for
/// the life of the process.
pub const BLE_IDENTIFIER_RETAINED_LIMIT: usize = 128;

/// Retained capacity limit for a characteristic value, in bytes.
///
/// Twice the 512-byte maximum attribute value length the ATT specification
/// permits, so legal traffic — which cannot exceed the negotiated MTU anyway —
/// never gives the buffer back and never re-grows it. Chosen as a multiple of
/// the protocol's own bound rather than of an observed payload size, because a
/// limit set just above what one peripheral sends is a reallocation on every
/// notification for the next one.
pub const BLE_VALUE_RETAINED_LIMIT: usize = 1024;

/// BLE characteristic value change payload, carried in a pooled slot inside
/// `HostCommand::OnBLECharacteristicValueChange`.
///
/// Three identifier strings plus a variable-length value, kept out of the enum
/// itself so `HostCommand` stays small on the channel.
///
/// **The fields are private and the only way to fill one is [`Self::overwrite`],
/// which is the invariant rather than encapsulation for its own sake.** A
/// notification stream runs at whatever rate the peripheral chooses — a hundred
/// hertz is ordinary — and Section 7.3 forbids a per-event allocation on it. A
/// public `String` field invites `device_id: id.to_owned()`, which reads as
/// obviously correct and allocates on every notification of every stream. There
/// is no way to write that here.
#[derive(Debug, Default)]
pub struct BleCharacteristicData {
    device_id: String,
    service_id: String,
    characteristic_id: String,
    value: Vec<u8>,
}

impl BleCharacteristicData {
    /// Replace the contents in place, reusing the buffers this slot already owns.
    ///
    /// `clear` keeps the capacity, so a slot that has carried one notification
    /// carries every later one of the same shape without touching the heap.
    pub fn overwrite(
        &mut self,
        device_id: &str,
        service_id: &str,
        characteristic_id: &str,
        value: &[u8],
    ) {
        self.device_id.clear();
        self.device_id.push_str(device_id);
        self.service_id.clear();
        self.service_id.push_str(service_id);
        self.characteristic_id.clear();
        self.characteristic_id.push_str(characteristic_id);
        self.value.clear();
        self.value.extend_from_slice(value);
    }

    #[must_use]
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    #[must_use]
    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    #[must_use]
    pub fn characteristic_id(&self) -> &str {
        &self.characteristic_id
    }

    #[must_use]
    pub fn value(&self) -> &[u8] {
        &self.value
    }

    /// Heap bytes this payload is holding on behalf of its pool.
    ///
    /// Capacity rather than length: a recycled slot is empty, so what it costs
    /// the Session is the buffers it kept for the next notification.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        self.device_id.capacity()
            + self.service_id.capacity()
            + self.characteristic_id.capacity()
            + self.value.capacity()
    }
}

impl crate::payload_pool::Recyclable for BleCharacteristicData {
    /// Empty the payload, keeping every buffer within its retained limit.
    ///
    /// Releasing an over-limit buffer outright rather than shrinking it is the
    /// point: `shrink_to` reallocates, and this runs on the Host thread as the
    /// consumer finishes with the notification. A release is a free, so the one
    /// path that reacts to a malformed event costs nothing that a gate counts.
    fn recycle(&mut self) {
        retain_string_within(&mut self.device_id, BLE_IDENTIFIER_RETAINED_LIMIT);
        retain_string_within(&mut self.service_id, BLE_IDENTIFIER_RETAINED_LIMIT);
        retain_string_within(&mut self.characteristic_id, BLE_IDENTIFIER_RETAINED_LIMIT);
        if self.value.capacity() > BLE_VALUE_RETAINED_LIMIT {
            self.value = Vec::new();
        } else {
            self.value.clear();
        }
    }
}

fn retain_string_within(buffer: &mut String, limit: usize) {
    if buffer.capacity() > limit {
        *buffer = String::new();
    } else {
        buffer.clear();
    }
}

/// Commands sent to the host runtime thread.
///
/// These commands drive the JavaScript runtime and coordinate between
/// native subsystems (rendering, audio, input) and the JS game code.
///
/// # Variant Groups
///
/// The enum below is the authoritative list; the grouping and counts here are
/// indicative and may lag as variants (e.g. `SurfaceDestroyed`) are added.
///
/// - **Module Loading** (2): `EvaluateModule`, `EvalScript`
/// - **Lifecycle** (4): `Restart`, `Shutdown`, `OnShow`, `OnHide`
/// - **Rendering / Surface** (1): `UpdateSurface`
/// - **Touch / Input** (1): `OnTouch`
/// - **Desktop pointer** (4): `OnMouseDown` .. `OnWheel`
/// - **Keyboard Events** (6): `OnKeyboardInput` .. `OnKeyUp`
/// - **Gamepad Events** (3): `OnGamepadConnected` .. `OnGamepadState`
/// - **IME Composition Events** (3): `OnCompositionStart` .. `OnCompositionEnd`
/// - **Sensor Events** (5): `OnDeviceMotionChange` .. `OnAccelerometerChange`
/// - **Network** (1): `OnNetworkStatusChange`
/// - **Audio Events** (3): `OnAudioInterruptionBegin`, `OnAudioInterruptionEnd`, `InnerAudioEvent`
/// - **Recorder Events** (2): `RecorderEvent`, `RecorderFrameData`
/// - **Camera Events** (2): `CameraEvent`, `CameraFrameData`
/// - **Bluetooth / BLE Events** (7): `OnBluetoothAdapterStateChange` .. `OnBeaconServiceChange`
/// - **Video Events** (1): `OnVideoStateChange`
/// - **System Events** (3): `OnMemoryWarning`, `OnUserCaptureScreen`, `OnThermalStatusChanged`
///
/// # Thread Safety
///
/// Commands are sent via `tokio::sync::mpsc::Sender` and processed
/// asynchronously by the host thread's event loop.
///
/// # Example
///
/// ```rust,ignore
/// use shared::protocol::host_cmd::HostCommand;
///
/// // Start a game
/// let cmd = HostCommand::EvaluateModule {
///     dir: “/data/game”.to_string(),
///     entry: “main.js”.to_string(),
/// };
///
/// // Send touch event
/// let cmd = HostCommand::OnTouch(Box::new(TouchData {
///     touch_type: TouchType::Start,
///     count: 1,
///     points: Default::default(), // filled via ptr::copy_nonoverlapping
///     timestamp_ms: 1234567890,
/// }));
/// ```
#[non_exhaustive]
#[derive(Debug)]
pub enum HostCommand {
    // ---- Module Loading ----
    /// Evaluate an ES module with isolated VFS paths.
    ///
    /// This is the primary way to start a mini-game. The module is loaded
    /// from the game's code directory, and sandboxed file access is enabled.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// HostCommand::EvaluateModule {
    ///     game_id: “my-puzzle-game”.to_string(),
    ///     entry: “game.js”.to_string(),
    /// }
    /// ```
    EvaluateModule {
        /// Unique game identifier (1-64 lower-case alphanumeric, underscore, hyphen).
        /// Used to derive all game paths from base directories.
        game_id: String,
        /// Entry point module filename (e.g., “main.js”, “game.js”).
        entry: String,
    },

    /// Evaluate a JS snippet (non-module).
    ///
    /// Useful for debugging or dynamic code execution. The code runs
    /// in the global scope, not as a module. Also used by Mode C async
    /// APIs (EvalScript pattern) to deliver platform callback results.
    EvalScript {
        /// JavaScript source code to execute.
        source: String,
    },

    /// Call one host-bridge hook by name, arguments encoded as a JSON array.
    ///
    /// This is how every host callback is delivered. It replaced [`Self::EvalScript`]
    /// source, which had to name the hook's holder -- and named it with a symbol
    /// from the *global* registry, so content could ask for the same one and call
    /// any of the 78 hooks behind it, rewarded-video completion included.
    ///
    /// Dispatching through a handle the runtime resolved at start-up needs no
    /// name, so there is no longer one to install: `js_bindings` deletes it as
    /// soon as it has resolved it.
    ///
    /// [`Self::EvalScript`] remains for the embedder's own `executeScript`,
    /// which is the host running its own code and not a callback channel.
    InvokeHostHook {
        /// Hook name on the host bridge, e.g. `_internalOnLoginResult`.
        ///
        /// `&'static str` on purpose: every hook name is a literal chosen by
        /// the runtime, and a type that cannot hold a computed string cannot
        /// carry one that came from somewhere else.
        hook: &'static str,
        /// Arguments as a JSON array. Covers every shape in use: `[]` for no
        /// arguments, `["{...}"]` for a JSON string, `[{...}]` for an object,
        /// `[1, 0]` for numbers.
        args_json: Cow<'static, str>,
    },

    // ---- Lifecycle Events ----
    /// Restart the current game runtime.
    ///
    /// Re-initializes the JS runtime and re-loads the last evaluated module
    /// (or does nothing if none has been evaluated). This is analogous to
    /// mini-program “restart” behavior.
    Restart,

    /// Gracefully shut down the host thread.
    ///
    /// This triggers cleanup of all resources and terminates the event loop.
    Shutdown,

    /// Notify that the app has become visible/active.
    ///
    /// Triggers `migo.onShow` callbacks in the game.
    ///
    /// `options_json` should be a JSON object string with launch/enter params
    /// (e.g. scene/query/referrerInfo/shareTicket). If absent or invalid, JS
    /// will fall back to default launch options.
    OnShow { options_json: Option<String> },

    /// Notify that the app has become hidden/inactive.
    ///
    /// Triggers `migo.onHide` callbacks in the game.
    OnHide,

    /// Window/input focus changed independently of app visibility.
    ///
    /// The runtime forwards this into a profile-neutral adapter hook. HTML5
    /// adapters translate it to window focus/blur; the common mini-game platform leaves it as
    /// retained engine state because that platform has no equivalent public API.
    OnFocusChanged { focused: bool },

    // ---- Audio Events ----
    /// Notify that audio playback has been interrupted.
    ///
    /// Triggered when the system takes audio focus (e.g., incoming call).
    OnAudioInterruptionBegin,

    /// Notify that audio interruption has ended.
    ///
    /// Triggered when the system returns audio focus.
    OnAudioInterruptionEnd,

    /// InnerAudioContext event pushed from audio thread.
    ///
    /// Used to notify the JS layer of audio playback state changes.
    InnerAudioEvent {
        /// The InnerAudioContext instance ID.
        id: u32,
        /// Type of event (play, pause, ended, etc.).
        event_type: InnerAudioEventType,
        /// Current playback position in seconds.
        current_time: f64,
    },

    // ---- Rendering / Surface ----
    /// Update the rendering surface (e.g., after orientation change).
    ///
    /// The render thread will recreate the EGL context with the new surface.
    UpdateSurface {
        /// Generation-tagged Surface lease from the platform layer.
        lease: SurfaceLease,
        /// Host-authoritative DPR update. `None` preserves the current ratio.
        pixel_ratio: Option<PixelRatio>,
    },

    /// Notify that the current rendering surface has been destroyed.
    SurfaceDestroyed {
        /// Exact retired generation; delayed commands cannot affect a newer one.
        generation: SurfaceGeneration,
    },

    /// The renderer installed the Surface a publication asked it to.
    ///
    /// Sent on every successful install, not only the ones whose reply was lost. The
    /// session commits its attachment slot from the reply when the reply arrives in
    /// time and from this when it does not -- and `RenderService` gives up on that
    /// reply after 500 ms while the request stays queued, so "in time" is not
    /// something either side can promise. Committing twice is a no-op; committing
    /// never left the session believing it had no Surface while the renderer held a
    /// live one, and the renderer paused with nothing coming.
    SurfaceInstalled { revision: SurfaceCandidateRevision },

    /// The renderer retired a still-live Surface after an unrecoverable native
    /// presentation failure. Expected host detach never emits this command.
    SurfaceLost {
        public_generation: PublicSurfaceGeneration,
        reason: SurfaceLossReason,
    },

    // ---- Touch / Input ----
    /// Dispatch touch input events to the game.
    ///
    /// Stored in a bounded, preallocated payload slot. Up to 512 normal
    /// commands can be pending, so enum size directly affects queue memory.
    OnTouch(Pooled<TouchData>),

    // ---- Sensor Events ----
    /// Device motion sensor data (rotation angles from TYPE_ROTATION_VECTOR).
    ///
    /// Sent by the platform sensor listener at the requested interval.
    /// Values follow the W3C DeviceOrientation spec:
    /// alpha = rotation around Z (0-360), beta = X (-180..180), gamma = Y (-90..90).
    OnDeviceMotionChange {
        /// Rotation around Z, 0-360.
        alpha: f64,
        /// Rotation around X, -180..180.
        beta: f64,
        /// Rotation around Y, -90..90.
        gamma: f64,
        /// See [`HostCommand::callback_generation`].
        runtime_generation: Option<NonZeroI64>,
    },

    /// Gyroscope sensor data (angular velocity in rad/s).
    ///
    /// Sent by the platform gyroscope listener at the requested interval.
    OnGyroscopeChange {
        /// Angular velocity around X in rad/s.
        x: f64,
        /// Angular velocity around Y in rad/s.
        y: f64,
        /// Angular velocity around Z in rad/s.
        z: f64,
        /// See [`HostCommand::callback_generation`].
        runtime_generation: Option<NonZeroI64>,
    },

    /// Device screen orientation changed (portrait/landscape).
    ///
    /// Sent by the platform when the display orientation changes.
    OnDeviceOrientationChange {
        /// One of: “portrait”, “landscape”, “landscapeReverse”.
        value: Cow<'static, str>,
    },

    /// Compass data (direction and accuracy).
    ///
    /// Sent by the platform compass listener (~5 times/second).
    OnCompassChange {
        /// Direction in degrees (0-360, 0 = north).
        direction: f64,
        /// Accuracy string (Android: “high”/”medium”/”low”/”no-contact”/”unreliable”).
        accuracy: Cow<'static, str>,
        /// See [`HostCommand::callback_generation`].
        runtime_generation: Option<NonZeroI64>,
    },

    /// Accelerometer data (acceleration in m/s^2).
    ///
    /// Sent by the platform accelerometer listener at the requested interval.
    OnAccelerometerChange {
        /// Acceleration along X axis in m/s^2.
        x: f64,
        /// Acceleration along Y axis in m/s^2.
        y: f64,
        /// Acceleration along Z axis in m/s^2.
        z: f64,
        /// See [`HostCommand::callback_generation`].
        runtime_generation: Option<NonZeroI64>,
    },

    // ---- Network Events ----
    /// Network status changed.
    ///
    /// Sent by the platform network monitor when connectivity changes.
    OnNetworkStatusChange {
        /// Whether network is connected.
        is_connected: bool,
        /// Network type: “wifi”, “2g”, “3g”, “4g”, “5g”, “unknown”, “none”.
        network_type: Cow<'static, str>,
    },

    // ---- Recorder Events ----
    /// Recorder event pushed from platform (start, pause, resume, stop, error, interruption).
    RecorderEvent {
        /// Event type string (e.g., “start”, “stop”, “error”, “interruptionBegin”).
        event_type: String,
        /// JSON-encoded payload (e.g., stop result with tempFilePath/duration/fileSize).
        json_payload: String,
        /// See [`HostCommand::callback_generation`].
        runtime_generation: Option<NonZeroI64>,
    },

    /// Recorder frame data pushed from platform (for onFrameRecorded).
    RecorderFrameData {
        /// Raw PCM/encoded audio frame bytes.
        data: Vec<u8>,
        /// Whether this is the last frame before stop.
        is_last_frame: bool,
        /// See [`HostCommand::callback_generation`].
        runtime_generation: Option<NonZeroI64>,
    },

    // ---- Camera Events ----
    /// Camera event pushed from platform (stop, authCancel, error, timeoutCallback).
    CameraEvent {
        /// JS-assigned camera instance ID.
        camera_id: u32,
        /// Event type string (e.g., “stop”, “authCancel”, “error”, “timeoutCallback”).
        event_type: String,
        /// JSON-encoded payload.
        json_payload: String,
        /// See [`HostCommand::callback_generation`].
        runtime_generation: Option<NonZeroI64>,
    },

    /// Camera frame data pushed from platform (for onCameraFrame / listenFrameChange).
    CameraFrameData {
        /// JS-assigned camera instance ID.
        camera_id: u32,
        /// Raw Y/U/V plane-window bytes concatenated in Y, U, V order (each
        /// plane's `position..limit`), exactly as delivered to JS.
        data: Vec<u8>,
        /// Frame width in pixels.
        width: u32,
        /// Frame height in pixels.
        height: u32,
        /// See [`HostCommand::callback_generation`].
        runtime_generation: Option<NonZeroI64>,
    },

    // ---- Keyboard Events ----
    /// Keyboard input event (user typed text in soft keyboard).
    ///
    /// Triggers `migo.onKeyboardInput` callbacks.
    OnKeyboardInput {
        /// Current text value of the keyboard input.
        value: String,
        /// See [`HostCommand::callback_generation`].
        runtime_generation: Option<NonZeroI64>,
    },

    /// Keyboard height changed (soft keyboard shown/hidden or resized).
    ///
    /// Triggers `migo.onKeyboardHeightChange` callbacks.
    OnKeyboardHeightChange {
        /// Keyboard height in CSS pixels (0 when hidden).
        height: f64,
        /// See [`HostCommand::callback_generation`].
        runtime_generation: Option<NonZeroI64>,
    },

    /// User pressed the confirm button on the soft keyboard.
    ///
    /// Triggers `migo.onKeyboardConfirm` callbacks.
    OnKeyboardConfirm {
        /// Current text value of the keyboard input.
        value: String,
        /// See [`HostCommand::callback_generation`].
        runtime_generation: Option<NonZeroI64>,
    },

    /// Soft keyboard dismissed/completed.
    ///
    /// Triggers `migo.onKeyboardComplete` callbacks.
    OnKeyboardComplete {
        /// Current text value of the keyboard input.
        value: String,
        /// See [`HostCommand::callback_generation`].
        runtime_generation: Option<NonZeroI64>,
    },

    /// Physical/PC keyboard key down event.
    ///
    /// Triggers `migo.onKeyDown` callbacks.
    OnKeyDown {
        /// Web KeyEvent.key value.
        key: String,
        /// Web KeyEvent.code value.
        code: String,
        /// Event timestamp in milliseconds.
        timestamp_ms: f64,
        /// DOM modifier state as a bitmask. `key` alone cannot carry it: a
        /// modified press still reports the character it produces, so content
        /// could not tell `Ctrl+S` from `S`.
        modifiers: u32,
        /// DOM `KeyboardEvent.repeat`: the platform's auto-repeat produced this
        /// press, rather than the user pressing the key again.
        repeat: bool,
    },

    /// Physical/PC keyboard key up event.
    ///
    /// Triggers `migo.onKeyUp` callbacks.
    OnKeyUp {
        /// Web KeyEvent.key value.
        key: String,
        /// Web KeyEvent.code value.
        code: String,
        /// Event timestamp in milliseconds.
        timestamp_ms: f64,
        /// DOM modifier state as a bitmask.
        modifiers: u32,
        /// DOM `KeyboardEvent.repeat`. Always false for a release on every
        /// platform that reports one, but carried so the two commands keep the
        /// same shape.
        repeat: bool,
    },

    // ---- Desktop pointer ----
    /// Mouse button pressed.
    ///
    /// Triggers `migo.onMouseDown` callbacks. Distinct from `OnTouch`, and a
    /// host chooses which it sends: mini-game content written for a phone listens for
    /// touch, content written for a PC mini-game platform listens for the mouse, and only
    /// the host knows which streams its content and its device call for. The
    /// engine synthesizes neither from the other.
    OnMouseDown {
        /// CSS pixels, the same logical coordinate space as `OnTouch`.
        x: f32,
        /// CSS pixels, the same logical coordinate space as `OnTouch`.
        y: f32,
        /// Which button, in DOM `MouseEvent.button` order (0 = primary).
        button: u32,
        /// Event timestamp in milliseconds.
        timestamp_ms: f64,
    },

    /// Mouse moved.
    ///
    /// Triggers `migo.onMouseMove` callbacks.
    OnMouseMove {
        /// CSS pixels, the same logical coordinate space as `OnTouch`.
        x: f32,
        /// CSS pixels, the same logical coordinate space as `OnTouch`.
        y: f32,
        /// Which button is held, in DOM `MouseEvent.button` order.
        button: u32,
        /// Event timestamp in milliseconds.
        timestamp_ms: f64,
    },

    /// Mouse button released.
    ///
    /// Triggers `migo.onMouseUp` callbacks.
    OnMouseUp {
        /// CSS pixels, the same logical coordinate space as `OnTouch`.
        x: f32,
        /// CSS pixels, the same logical coordinate space as `OnTouch`.
        y: f32,
        /// Which button, in DOM `MouseEvent.button` order (0 = primary).
        button: u32,
        /// Event timestamp in milliseconds.
        timestamp_ms: f64,
    },

    /// Wheel or trackpad scroll.
    ///
    /// Triggers `migo.onWheel` callbacks. The wheel has no touch equivalent, so
    /// unlike the mouse buttons above there is no other stream that could carry
    /// it.
    OnWheel {
        /// Horizontal delta, in the unit `delta_mode` names.
        delta_x: f64,
        /// Vertical delta, in the unit `delta_mode` names.
        delta_y: f64,
        /// Depth delta, in the unit `delta_mode` names; 0.0 on most devices.
        delta_z: f64,
        /// DOM `WheelEvent.deltaMode`: 0 = pixel, 1 = line, 2 = page. Carried
        /// rather than normalized to pixels because converting a line-based
        /// delta needs the content's line height, which only the content knows.
        delta_mode: u32,
        /// Event timestamp in milliseconds.
        timestamp_ms: f64,
    },

    // ---- IME Composition Events ----
    /// The user began composing text through an IME.
    ///
    /// Triggers `compositionstart`. Composition is the in-progress state of IME
    /// input -- typing pinyin shows a preedit string before any of it is
    /// committed -- and it is distinct from the soft keyboard's
    /// `OnKeyboardInput`, which reports text that has already been committed. A
    /// game drawing its own text field needs both: the preedit to show what is
    /// being typed, and the committed value to store.
    OnCompositionStart {
        /// The preedit text at the moment composition began; usually empty.
        data: String,
    },

    /// The preedit text changed.
    ///
    /// Triggers `compositionupdate`. `data` is the whole current preedit
    /// string, not the delta -- the same rule the soft keyboard's value follows,
    /// and for the same reason: a host sending only what changed leaves content
    /// unable to reconstruct the rest.
    OnCompositionUpdate { data: String },

    /// Composition finished.
    ///
    /// Triggers `compositionend`. `data` is the committed text, which is empty
    /// when the user cancelled. Content that has been drawing the preedit must
    /// clear it on this event; the committed text arrives here and, for a host
    /// that also drives the soft keyboard, again as an input value.
    OnCompositionEnd { data: String },

    // ---- Gamepad Events ----
    /// A gamepad became available in `index`.
    ///
    /// Triggers a `gamepadconnected` event. The id is the device's own name,
    /// which content shows to a player and uses to recognise a known pad.
    OnGamepadConnected {
        /// The slot the pad occupies for as long as it stays connected.
        index: u32,
        /// Human-readable device name, as the Web API's `Gamepad.id`.
        id: String,
        /// `"standard"` when the host mapped the pad onto the standard layout,
        /// and empty when it did not -- content reads it to decide whether the
        /// button order can be trusted.
        mapping: String,
        /// How many axes and buttons this pad reports.
        ///
        /// Carried on connect rather than inferred from the first state sample
        /// because `getGamepads()` must return correctly sized arrays from the
        /// moment the `gamepadconnected` listener runs -- content commonly reads
        /// `buttons.length` there to decide which layout it is looking at.
        axis_count: u8,
        button_count: u8,
    },

    /// The gamepad in `index` went away.
    ///
    /// Triggers a `gamepaddisconnected` event and empties the slot, so content
    /// polling `getGamepads()` sees a hole rather than a stale pad.
    OnGamepadDisconnected { index: u32 },

    /// New axis and button values for a connected gamepad.
    ///
    /// The Web API is polled rather than evented: content calls
    /// `getGamepads()` each frame and reads whatever is current. So this
    /// updates stored state instead of dispatching, and a host sends it as
    /// often as it samples.
    OnGamepadState(Pooled<GamepadState>),

    // ---- Bluetooth / BLE Events ----
    /// Bluetooth adapter state changed (available/discovering).
    ///
    /// Triggers `migo.onBluetoothAdapterStateChange` callbacks.
    OnBluetoothAdapterStateChange {
        /// Whether Bluetooth adapter is available.
        available: bool,
        /// Whether Bluetooth adapter is discovering devices.
        discovering: bool,
    },

    /// New Bluetooth device(s) found during discovery.
    ///
    /// Triggers `migo.onBluetoothDeviceFound` callbacks.
    OnBluetoothDeviceFound {
        /// JSON-encoded array of discovered devices.
        devices_json: String,
    },

    /// BLE connection state changed (connected/disconnected).
    ///
    /// Triggers `migo.onBLEConnectionStateChange` callbacks.
    OnBLEConnectionStateChange {
        /// BLE device identifier.
        device_id: String,
        /// Whether the device is connected.
        connected: bool,
    },

    /// BLE characteristic value changed (notification/indication received).
    ///
    /// Carried in a pooled slot, which keeps the `HostCommand` enum small and
    /// keeps the payload's buffers off the heap between notifications: the slot
    /// returns to its Session's pool when the Host thread drops this command.
    /// Triggers `migo.onBLECharacteristicValueChange` callbacks.
    OnBLECharacteristicValueChange(Recycled<BleCharacteristicData>),

    /// BLE MTU changed after negotiation.
    ///
    /// Triggers `migo.onBLEMTUChange` callbacks.
    OnBLEMTUChange {
        /// BLE device identifier.
        device_id: String,
        /// New MTU value.
        mtu: u32,
    },

    /// Beacon devices updated during discovery.
    ///
    /// Triggers `migo.onBeaconUpdate` callbacks.
    OnBeaconUpdate {
        /// JSON-encoded array of beacon devices.
        beacons_json: String,
    },

    /// Beacon service state changed.
    ///
    /// Triggers `migo.onBeaconServiceChange` callbacks.
    OnBeaconServiceChange {
        /// Whether beacon service is available.
        available: bool,
        /// Whether beacon service is currently discovering.
        discovering: bool,
    },

    // ---- Video Events ----
    /// Video player state change event.
    ///
    /// Uses a single variant with `event_type` to cover all video events:
    /// "play", "pause", "ended", "timeupdate", "waiting", "progress",
    /// "error", "fullscreenchange".
    ///
    /// Triggers the corresponding event listener on the Video instance
    /// identified by `video_id`.
    OnVideoStateChange {
        /// The video player instance ID.
        video_id: u32,
        /// Event type: "play", "pause", "ended", "timeupdate", "waiting",
        /// "progress", "error", "fullscreenchange".
        event_type: String,
        /// JSON-encoded event data (e.g. `{"currentTime":12.5}` for timeupdate,
        /// `{"errMsg":"..."}` for error, `{"fullScreen":true,"direction":0}` for
        /// fullscreenchange).
        data: String,
        /// See [`HostCommand::callback_generation`].
        runtime_generation: Option<NonZeroI64>,
    },

    // ---- System Events ----
    /// Memory warning from the system.
    ///
    /// Triggered when Android sends `onTrimMemory` or `onLowMemory`.
    /// Triggers `migo.onMemoryWarning` callbacks in the game.
    OnMemoryWarning {
        /// Memory warning level (Android TRIM_MEMORY_* constants):
        /// 5 = RUNNING_MODERATE, 10 = RUNNING_LOW, 15 = RUNNING_CRITICAL.
        level: i32,
    },

    /// User took a screenshot (system screenshot button pressed).
    ///
    /// Triggers `migo.onUserCaptureScreen` callback in the game.
    OnUserCaptureScreen {
        /// See [`HostCommand::callback_generation`].
        runtime_generation: Option<NonZeroI64>,
    },

    /// Thermal status changed (ADPF, API 29+).
    /// Level 0=none, 1=light, 2=moderate, 3=severe, 4=critical, 5=emergency, 6=shutdown.
    OnThermalStatusChanged { status: i32 },

    // ---- Host Message Channel ----
    /// Message from game JS to host app via migo.sendToHost(type, payload).
    ///
    /// `json` is a `{"type":"...","payload":"..."}` envelope.
    /// The host thread forwards this to `PlatformServices::notify_host_message`,
    /// which calls `NativeExports.onHostMessage` via JNI outbound.
    SendToHost { json: String },
}

/// Read a generation a platform captured, rejecting anything a token cannot be.
///
/// Platforms that carry the generation over a boundary with no "absent" — an
/// Android `long` across JNI, a C `int64_t` — signal *unfenced* with a
/// non-positive value. Trusting it as a generation instead would compare a
/// producer that captured nothing against a real one, and `0 != current` reads
/// as retired: every event from an unfenced producer would be silently dropped.
///
/// One implementation because every producer needs the same answer, and there
/// are about twenty more of them to fence.
///
/// The result is `NonZeroI64` so that "zero is not a generation" stops being a
/// rule this function enforces and becomes something the type cannot express.
/// It is also why the fence costs nothing: `Option<NonZeroI64>` is eight bytes
/// against `Option<i64>`'s sixteen, and `HostCommand` sits exactly on the 64-byte
/// cap its own assertion pins, with 512 of them preallocated per queue. Every one
/// of the twenty-odd producers still to be fenced adds this field to its variant.
pub fn captured_generation(generation: i64) -> Option<NonZeroI64> {
    NonZeroI64::new(generation).filter(|value| value.get() > 0)
}

impl HostCommand {
    /// The runtime generation this command was produced *for*, when its producer
    /// captured one.
    ///
    /// A runtime restart replaces the JavaScript isolate but not the platform
    /// objects around it: an Android manager, a proxy Activity, a C host's
    /// keyboard. Those keep producing events, and an event aimed at the isolate
    /// that has just been retired must not be delivered to the one that replaced
    /// it. The generation is what tells them apart, and it has to be *captured
    /// where the event was produced* — reading the current one at enqueue would
    /// always match and prove nothing.
    ///
    /// `None` means this command is not subject to that check, for one of two
    /// reasons that are deliberately not distinguished here:
    ///
    /// * it is not a runtime-owned callback at all — `Restart`, `UpdateSurface`,
    ///   a touch, a physical key. Dropping a key *up* because a restart happened
    ///   would leave content believing the key is still held, which is a worse
    ///   failure than delivering it late;
    /// * or its producer has not been fenced yet. Android's managers are in that
    ///   state until this plan's task 7 gives them tokens, and `None` says so
    ///   out loud rather than passing the current generation and pretending.
    ///
    /// The match is exhaustive **by construction**: there is no wildcard, so a
    /// new command cannot be added without deciding which of the two it is.
    pub fn callback_generation(&self) -> Option<NonZeroI64> {
        match self {
            Self::OnKeyboardInput {
                runtime_generation, ..
            }
            | Self::OnKeyboardHeightChange {
                runtime_generation, ..
            }
            | Self::OnKeyboardConfirm {
                runtime_generation, ..
            }
            | Self::OnKeyboardComplete {
                runtime_generation, ..
            }
            // The sensor streams and the screenshot observer: Android listeners
            // that stay registered across a restart and keep firing. Physical
            // orientation is deliberately *not* here -- a rotation reported after
            // a restart is a current fact about the device, and the replacement
            // isolate needs it as much as the retired one did.
            | Self::OnDeviceMotionChange {
                runtime_generation, ..
            }
            | Self::OnGyroscopeChange {
                runtime_generation, ..
            }
            | Self::OnCompassChange {
                runtime_generation, ..
            }
            | Self::OnAccelerometerChange {
                runtime_generation, ..
            }
            | Self::OnUserCaptureScreen { runtime_generation }
            // Camera, microphone and video: the managers that hold hardware.
            // Their events and their frames both stop belonging to anything the
            // moment the isolate that opened them is replaced, and the teardown
            // that releases the hardware reports as it goes.
            | Self::CameraEvent {
                runtime_generation, ..
            }
            | Self::CameraFrameData {
                runtime_generation, ..
            }
            | Self::RecorderEvent {
                runtime_generation, ..
            }
            | Self::RecorderFrameData {
                runtime_generation, ..
            }
            | Self::OnVideoStateChange {
                runtime_generation, ..
            } => *runtime_generation,

            Self::EvaluateModule { .. }
            | Self::EvalScript { .. }
            | Self::InvokeHostHook { .. }
            | Self::Restart
            | Self::Shutdown
            | Self::OnShow { .. }
            | Self::OnHide
            | Self::OnFocusChanged { .. }
            | Self::OnAudioInterruptionBegin
            | Self::OnAudioInterruptionEnd
            | Self::InnerAudioEvent { .. }
            | Self::UpdateSurface { .. }
            | Self::SurfaceDestroyed { .. }
            | Self::SurfaceInstalled { .. }
            | Self::SurfaceLost { .. }
            | Self::OnTouch(..)
            | Self::OnDeviceOrientationChange { .. }
            | Self::OnNetworkStatusChange { .. }
            | Self::OnKeyDown { .. }
            | Self::OnKeyUp { .. }
            | Self::OnMouseDown { .. }
            | Self::OnMouseMove { .. }
            | Self::OnMouseUp { .. }
            | Self::OnWheel { .. }
            | Self::OnCompositionStart { .. }
            | Self::OnCompositionUpdate { .. }
            | Self::OnCompositionEnd { .. }
            | Self::OnGamepadConnected { .. }
            | Self::OnGamepadDisconnected { .. }
            | Self::OnGamepadState(..)
            | Self::OnBluetoothAdapterStateChange { .. }
            | Self::OnBluetoothDeviceFound { .. }
            | Self::OnBLEConnectionStateChange { .. }
            | Self::OnBLECharacteristicValueChange(..)
            | Self::OnBLEMTUChange { .. }
            | Self::OnBeaconUpdate { .. }
            | Self::OnBeaconServiceChange { .. }
            | Self::OnMemoryWarning { .. }
            | Self::OnThermalStatusChanged { .. }
            | Self::SendToHost { .. } => None,
        }
    }
}

// Guard against future regressions — if a new variant re-inflates the enum,
// this assertion will fail at compile time.
const _: () = assert!(
    core::mem::size_of::<HostCommand>() <= 64,
    "HostCommand grew past 64 bytes; check for unboxed large variants"
);

// The fence field is `Option<NonZeroI64>` for this: it is the same eight bytes a
// bare `i64` would cost, so fencing a variant does not spend the headroom the
// assertion above is down to. `Option<i64>` would be sixteen, on every one of the
// twenty-odd producers still to be fenced.
const _: () = assert!(
    core::mem::size_of::<Option<NonZeroI64>>() == core::mem::size_of::<i64>(),
    "the fence field lost its niche optimisation and now costs a word per command"
);

/// Event types for InnerAudioContext.
///
/// These map to the Mini Program InnerAudioContext events:
/// - `onCanplay`, `onPlay`, `onPause`, `onStop`, `onEnded`
/// - `onSeeking`, `onSeeked`, `onTimeUpdate`, `onError`
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InnerAudioEventType {
    /// Audio is ready to play (enough data buffered).
    CanPlay,
    /// Playback has started.
    Play,
    /// Playback has paused.
    Pause,
    /// Playback has stopped (and reset to beginning).
    Stop,
    /// Playback reached the end of the audio.
    Ended,
    /// A seek operation has started.
    Seeking,
    /// A seek operation has completed.
    Seeked,
    /// Current playback time has updated (periodic).
    TimeUpdate,
    /// Playback is waiting for more data (streaming buffer underrun).
    Waiting,
    /// An error occurred during playback.
    Error,
}

impl InnerAudioEventType {
    /// Convert to JS-compatible camelCase string.
    ///
    /// Used when dispatching events to JavaScript callbacks.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let event = InnerAudioEventType::CanPlay;
    /// assert_eq!(event.as_str(), "canPlay");
    /// ```
    pub fn as_str(&self) -> &'static str {
        match self {
            InnerAudioEventType::CanPlay => "canPlay",
            InnerAudioEventType::Play => "play",
            InnerAudioEventType::Pause => "pause",
            InnerAudioEventType::Stop => "stop",
            InnerAudioEventType::Ended => "ended",
            InnerAudioEventType::Seeking => "seeking",
            InnerAudioEventType::Seeked => "seeked",
            InnerAudioEventType::TimeUpdate => "timeUpdate",
            InnerAudioEventType::Waiting => "waiting",
            InnerAudioEventType::Error => "error",
        }
    }
}

/// A single touch point in a multi-touch event.
///
/// Must match the memory layout produced by the Java side `ByteBuffer`.
/// The layout is carefully designed for zero-copy transfer across JNI.
///
/// # Memory Layout
///
/// ```text
/// Offset  Field     Type    Size
/// ------  --------  ------  ----
///   0     id        u32     4
///   4     x         f32     4
///   8     y         f32     4
///  12     pressure  f32     4
///  16     flags     u32     4
/// ------  --------  ------  ----
/// Total: 20 bytes, aligned to 4 bytes
/// ```
///
/// # Fields
///
/// - `id`: Unique identifier for the touch pointer (stable across move events)
/// - `x`, `y`: Touch coordinates in CSS pixels (logical, not physical)
/// - `pressure`: Pressure value from 0.0 (none) to 1.0 (maximum)
/// - `flags`: Bitfield describing this pointer for the current event:
///   bit 0 (`0x1`) = in `changedTouches`; bit 1 (`0x2`) = removed from the
///   surface this event (finger up / cancel), so it is excluded from `touches`.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct TouchPoint {
    /// Unique identifier for this touch pointer.
    /// Stable across the touch sequence (start → move → end).
    pub id: u32,
    /// X coordinate in CSS pixels (logical coordinates).
    pub x: f32,
    /// Y coordinate in CSS pixels (logical coordinates).
    pub y: f32,
    /// Touch pressure, normalized to 0.0–1.0 range.
    /// May be 0.0 if the device doesn't support pressure sensing.
    pub pressure: f32,
    /// Per-pointer bitfield: bit 0 = in `changedTouches`, bit 1 = removed from
    /// the surface this event (excluded from `touches`).
    pub flags: u32,
}

// Compile-time layout checks to prevent accidental ABI mismatch.
// These ensure the struct matches the Java/native side exactly.
// Must match Java's TOUCH_POINT_SIZE = 20 bytes.
const _: () = assert!(std::mem::size_of::<TouchPoint>() == 20);
const _: [(); 4] = [(); core::mem::align_of::<TouchPoint>()];

/// Type of touch event.
///
/// Maps to standard web/mobile touch event types.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TouchType {
    /// Touch started (finger down).
    Start = 0,
    /// Touch moved (finger dragged).
    Move = 1,
    /// Touch ended (finger up).
    End = 2,
    /// Touch cancelled (interrupted by system).
    Cancel = 3,
}

#[cfg(test)]
mod generation_tests {
    use super::{Cow, HostCommand, NonZeroI64, captured_generation};

    /// A generation as a producer would have captured it.
    fn generation(value: i64) -> Option<NonZeroI64> {
        NonZeroI64::new(value)
    }

    #[test]
    fn only_a_positive_value_is_a_captured_generation() {
        assert_eq!(captured_generation(1), generation(1));
        assert_eq!(captured_generation(i64::MAX), generation(i64::MAX));
        // The unfenced signal, and the values a corrupted or defaulted field
        // could hold. None of them may read as "produced for generation N",
        // because that would drop the event instead of delivering it.
        for unfenced in [0, -1, i64::MIN] {
            assert_eq!(captured_generation(unfenced), None, "{unfenced}");
        }
    }

    #[test]
    fn the_four_soft_keyboard_commands_carry_their_generation() {
        // Each is built in its own arm, and a stamp is exactly the sort of thing
        // that gets added to three of four.
        let commands = [
            HostCommand::OnKeyboardInput {
                value: String::new(),
                runtime_generation: generation(5),
            },
            HostCommand::OnKeyboardConfirm {
                value: String::new(),
                runtime_generation: generation(5),
            },
            HostCommand::OnKeyboardComplete {
                value: String::new(),
                runtime_generation: generation(5),
            },
            HostCommand::OnKeyboardHeightChange {
                height: 0.0,
                runtime_generation: generation(5),
            },
        ];
        for command in &commands {
            assert_eq!(command.callback_generation(), generation(5), "{command:?}");
        }
    }

    #[test]
    fn the_sensor_streams_and_the_screenshot_observer_carry_their_generation() {
        // Five arms, each written separately, and a stamp is exactly the sort of
        // thing that gets added to four of five.
        let commands = [
            HostCommand::OnDeviceMotionChange {
                alpha: 0.0,
                beta: 0.0,
                gamma: 0.0,
                runtime_generation: generation(9),
            },
            HostCommand::OnGyroscopeChange {
                x: 0.0,
                y: 0.0,
                z: 0.0,
                runtime_generation: generation(9),
            },
            HostCommand::OnCompassChange {
                direction: 0.0,
                accuracy: Cow::Borrowed("high"),
                runtime_generation: generation(9),
            },
            HostCommand::OnAccelerometerChange {
                x: 0.0,
                y: 0.0,
                z: 0.0,
                runtime_generation: generation(9),
            },
            HostCommand::OnUserCaptureScreen {
                runtime_generation: generation(9),
            },
        ];
        for command in &commands {
            assert_eq!(command.callback_generation(), generation(9), "{command:?}");
        }
    }

    #[test]
    fn the_hardware_managers_carry_their_generation_on_events_and_on_frames() {
        // Frames as well as events: a camera that keeps delivering into the
        // isolate that replaced the one which opened it is the same defect at
        // thirty times the rate.
        let commands = [
            HostCommand::CameraEvent {
                camera_id: 0,
                event_type: "stop".to_owned(),
                json_payload: "{}".to_owned(),
                runtime_generation: generation(4),
            },
            HostCommand::CameraFrameData {
                camera_id: 0,
                data: Vec::new(),
                width: 1,
                height: 1,
                runtime_generation: generation(4),
            },
            HostCommand::RecorderEvent {
                event_type: "stop".to_owned(),
                json_payload: "{}".to_owned(),
                runtime_generation: generation(4),
            },
            HostCommand::RecorderFrameData {
                data: Vec::new(),
                is_last_frame: true,
                runtime_generation: generation(4),
            },
            HostCommand::OnVideoStateChange {
                video_id: 0,
                event_type: "ended".to_owned(),
                data: "{}".to_owned(),
                runtime_generation: generation(4),
            },
        ];
        for command in &commands {
            assert_eq!(command.callback_generation(), generation(4), "{command:?}");
        }
    }

    #[test]
    fn a_synchronous_failure_reply_is_unfenced_and_therefore_delivered() {
        // An export that fails before any manager exists reports with the
        // `UNFENCED` zero the Java side stamps. It answers a call the live
        // runtime has just made, so it cannot be stale and must never be
        // dropped -- and zero must not be read as "produced for generation 0".
        let reply = HostCommand::RecorderEvent {
            event_type: "error".to_owned(),
            json_payload: "{}".to_owned(),
            runtime_generation: captured_generation(0),
        };
        assert_eq!(reply.callback_generation(), None);
    }

    #[test]
    fn a_device_orientation_change_is_never_fenced() {
        // The sensor manager does not produce it -- the Activity does, and a
        // rotation is a current fact about the device. Dropping one because a
        // restart happened would leave the replacement isolate believing the
        // screen is the way it was two runtimes ago, with nothing to correct it.
        // Same reasoning as a physical key going up.
        assert_eq!(
            HostCommand::OnDeviceOrientationChange {
                value: Cow::Borrowed("landscape"),
            }
            .callback_generation(),
            None
        );
    }

    #[test]
    fn a_command_that_is_not_a_runtime_callback_reports_no_generation() {
        // Dropping a key *up* because a restart happened leaves content
        // believing the key is still held, so these must never be fenced.
        assert_eq!(HostCommand::OnHide.callback_generation(), None);
        assert_eq!(
            HostCommand::OnKeyUp {
                key: "a".to_owned(),
                code: "KeyA".to_owned(),
                timestamp_ms: 0.0,
                modifiers: 0,
                repeat: false,
            }
            .callback_generation(),
            None
        );
    }
}
