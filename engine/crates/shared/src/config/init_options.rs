//! Initialization options for the Migo engine.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// Extra options for platform-specific or experimental features.
///
/// Stored as a JSON object for easier serialization and cross-language bridging.
/// This allows passing arbitrary key-value pairs from Java/Kotlin to Rust
/// without modifying the core API.
///
/// # Example
///
/// ```rust,ignore
/// use shared::config::Extras;
/// use serde_json::json;
///
/// let mut extras = Extras::new();
/// extras.insert("bluetooth_enabled".into(), json!(true));
/// extras.insert("custom_api_url".into(), json!("https://api.example.com"));
/// ```
pub type Extras = Map<String, Value>;

/// Log level for the engine.
///
/// Ordinals match Java `RuntimeConfig.LogLevel` enum exactly:
/// TRACE(0), DEBUG(1), INFO(2), WARN(3), ERROR(4), OFF(5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum LogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    #[default]
    Warn = 3,
    Error = 4,
    Off = 5,
}

impl LogLevel {
    /// The level a Java `RuntimeConfig.LogLevel` ordinal names, or `None` when
    /// the two enums do not agree on it.
    ///
    /// Separate from [`From<i32>`] because a conversion that cannot fail has to
    /// answer *something* for an unrecognised ordinal, and every answer is wrong
    /// in the same way: it makes "the host's SDK declares a level this library
    /// does not know" indistinguishable from "the host asked for Warn". That is
    /// a real state — the two enums are two copies of one list, in two
    /// languages, shipped in two artifacts a host may mix — and the caller that
    /// read the ordinal is the only place with somewhere to report it.
    pub fn from_ordinal(value: i32) -> Option<Self> {
        match value {
            0 => Some(LogLevel::Trace),
            1 => Some(LogLevel::Debug),
            2 => Some(LogLevel::Info),
            3 => Some(LogLevel::Warn),
            4 => Some(LogLevel::Error),
            5 => Some(LogLevel::Off),
            _ => None,
        }
    }
}

impl From<i32> for LogLevel {
    /// Kept for callers that have nowhere to report and nothing to decide.
    /// Prefer [`LogLevel::from_ordinal`] wherever an unrecognised ordinal is a
    /// fact worth saying out loud.
    fn from(value: i32) -> Self {
        LogLevel::from_ordinal(value).unwrap_or(LogLevel::Warn)
    }
}

/// Configuration options for initializing the Migo engine.
///
/// These options are typically provided by the platform layer (Java/Android)
/// and passed through JNI when creating a new host instance.
///
/// # Builder Pattern
///
/// Use the builder-style methods to construct options:
///
/// ```rust,ignore
/// use shared::config::InitOptions;
///
/// let options = InitOptions::new()
///     .with_pixel_ratio(2.0)
///     .with_tmp_dir("/data/app/cache".into())
///     .with_extra("feature_flag", true);
/// ```
///
/// # Platform Integration
///
/// On Android, these options are constructed from `InitOption.java`:
///
/// ```java
/// InitOption option = new InitOption.Builder(context)
///     .setFullScreen(true)
///     .setTargetFps(60)
///     .build();
/// ```
#[derive(Debug, Clone)]
pub struct InitOptions {
    /// Device pixel ratio (DPI scale factor).
    /// Values > 1.0 indicate high-DPI displays (e.g., 2.0 = Retina, 3.0 = xxhdpi).
    pixel_ratio: f32,
    /// Cache directory (may be cleared by system).
    /// - Android: `Context.getCacheDir()`
    /// - iOS: `NSCachesDirectory`
    cache_dir: PathBuf,
    /// Persistent files directory (survives app updates).
    /// - Android: `Context.getFilesDir()`
    /// - iOS: `NSDocumentDirectory`
    files_dir: PathBuf,
    /// Code cache directory for compiled scripts (V8 bytecode).
    code_cache_dir: PathBuf,
    /// Target frames per second; see [`crate::frame_rate`] for the range.
    target_fps: i32,
    /// Whether debug mode is enabled.
    debug_enabled: bool,
    /// Log level for the engine.
    log_level: LogLevel,
    /// Maximum memory limit for JavaScript runtime in MB.
    max_memory_mb: i32,
    /// Whether the ANR watchdog is enabled.
    /// When enabled, the process deadline scheduler guards each synchronous V8
    /// execution/poll and terminates an isolate that exceeds its budget.
    watchdog_enabled: bool,
    /// ANR watchdog timeout in seconds.
    /// If one guarded V8 execution does not return within this duration, the
    /// watchdog terminates the isolate and reports an ANR.
    /// Clamped to [5, 120]. Default: 10.
    watchdog_timeout_secs: i32,
    /// Whether code signing verification is enforced.
    ///
    /// When enabled, `code_signing_pubkey` must be provided and every module
    /// load will validate manifest signature + file hash before execution.
    code_signing_enabled: bool,
    /// Ed25519 public key for code signing verification (hex-encoded, 64 chars).
    /// Used only when `code_signing_enabled` is true.
    code_signing_pubkey: Option<String>,
    /// Subpackage definitions: list of (name, root) pairs.
    /// Provided by the host app via RuntimeConfig.
    sub_packages: Vec<(String, String)>,
    /// Workers directory path relative to code directory.
    /// Provided by the host app via RuntimeConfig.
    workers_path: Option<String>,
    /// Boot prelude scripts: `(name, source)` pairs evaluated **before**
    /// every `EvaluateModule` call.
    ///
    /// Useful for injecting a host-side BOM/DOM adapter (e.g. the
    /// `@minigame-labs/migo-adapter` IIFE bundle) so browser-style games
    /// can run unmodified, without touching the `migo.*` API surface.
    /// `name` is the script name shown in V8 stack traces; `source` is
    /// plain JavaScript (NOT an ES module).
    ///
    /// Scripts run in declaration order, in the global scope, before the
    /// game's main module is loaded. They are re-applied on every
    /// `Restart`-driven re-evaluation.
    prelude_scripts: Vec<(String, String)>,
    /// Platform-specific or experimental options.
    extras: Extras,
}

impl Default for InitOptions {
    fn default() -> Self {
        let temp = std::env::temp_dir();
        Self {
            pixel_ratio: 1.0,
            cache_dir: temp.clone(),
            files_dir: temp.clone(),
            code_cache_dir: temp,
            target_fps: crate::frame_rate::DEFAULT_FPS as i32,
            debug_enabled: false,
            log_level: LogLevel::Warn,
            max_memory_mb: 512,
            watchdog_enabled: true,
            watchdog_timeout_secs: 10,
            code_signing_enabled: true,
            code_signing_pubkey: None,
            sub_packages: Vec::new(),
            workers_path: None,
            prelude_scripts: Vec::new(),
            extras: Extras::new(),
        }
    }
}

impl InitOptions {
    /// Creates a new `InitOptions` with default values.
    ///
    /// Defaults:
    /// - `pixel_ratio`: 1.0
    /// - `tmp_dir`: System temp directory
    /// - `target_fps`: 60
    /// - `debug_enabled`: false
    /// - `log_level`: Warn
    /// - `max_memory_mb`: 512
    /// - `extras`: Empty map
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the device pixel ratio.
    ///
    /// This value is used to convert between logical (CSS) pixels and
    /// physical (device) pixels. A value of 2.0 means 1 CSS pixel = 2 device pixels.
    #[inline]
    pub fn pixel_ratio(&self) -> f32 {
        self.pixel_ratio
    }

    /// Returns the cache directory path.
    ///
    /// Used for caching decoded audio, images, intermediate files, etc.
    /// May be cleared by the system when storage is low.
    #[inline]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Returns the persistent files directory path.
    ///
    /// Used for user data, game saves, persistent configuration.
    /// Survives app updates and is not cleared by the system.
    #[inline]
    pub fn files_dir(&self) -> &Path {
        &self.files_dir
    }

    /// Alias for cache_dir for backward compatibility.
    #[inline]
    #[deprecated(note = "Use cache_dir() instead")]
    pub fn tmp_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Returns the code cache directory path.
    ///
    /// Used for caching compiled JavaScript bytecode.
    #[inline]
    pub fn code_cache_dir(&self) -> &Path {
        &self.code_cache_dir
    }

    /// The root the on-disk V8 code cache actually lives under.
    ///
    /// [`Self::code_cache_dir`] is what the host configured; this is what the
    /// runtime must use. The two were not connected: the C ABI and the Android
    /// bridge both plumb `code_cache_dir` all the way into `InitOptions`, and
    /// then `Host::new` handed `cache_dir()` to the runtime, so a host that
    /// deliberately pointed the code cache somewhere else -- a volume with room,
    /// a directory excluded from backup, the shared root the ABI documents as
    /// shared on purpose -- silently got a subdirectory of the ordinary cache
    /// instead, and nothing failed to say so.
    ///
    /// An unset (empty) value falls back to the cache directory, which is what
    /// every host that never set the option has been getting; the fallback is
    /// what keeps this from relocating their cache to a relative path under the
    /// process working directory.
    #[inline]
    pub fn code_cache_root(&self) -> &Path {
        if self.code_cache_dir.as_os_str().is_empty() {
            &self.cache_dir
        } else {
            &self.code_cache_dir
        }
    }

    /// Returns the target frames per second.
    #[inline]
    pub fn target_fps(&self) -> i32 {
        self.target_fps
    }

    /// Returns whether debug mode is enabled.
    #[inline]
    pub fn debug_enabled(&self) -> bool {
        self.debug_enabled
    }

    /// Returns the log level.
    #[inline]
    pub fn log_level(&self) -> LogLevel {
        self.log_level
    }

    /// Returns the maximum memory limit in MB.
    #[inline]
    pub fn max_memory_mb(&self) -> i32 {
        self.max_memory_mb
    }

    /// Returns whether the ANR watchdog is enabled.
    #[inline]
    pub fn watchdog_enabled(&self) -> bool {
        self.watchdog_enabled
    }

    /// Returns the ANR watchdog timeout in seconds.
    #[inline]
    pub fn watchdog_timeout_secs(&self) -> i32 {
        self.watchdog_timeout_secs
    }

    /// Returns whether code signing verification is enabled.
    #[inline]
    pub fn code_signing_enabled(&self) -> bool {
        self.code_signing_enabled
    }

    /// Returns the Ed25519 public key for code signing verification.
    /// Used only when `code_signing_enabled()` returns true.
    #[inline]
    pub fn code_signing_pubkey(&self) -> Option<&str> {
        self.code_signing_pubkey.as_deref()
    }

    /// Returns the subpackage definitions as (name, root) pairs.
    #[inline]
    pub fn sub_packages(&self) -> &[(String, String)] {
        &self.sub_packages
    }

    /// Returns the injected workers path, if any.
    #[inline]
    pub fn workers_path(&self) -> Option<&str> {
        self.workers_path.as_deref()
    }

    /// Returns the configured boot prelude scripts as `(name, source)` pairs.
    ///
    /// Each script runs in the global scope before `EvaluateModule` loads
    /// the game's main module. See [`Self::with_prelude_script`] for the
    /// intended use case (BOM/DOM adapter injection).
    #[inline]
    pub fn prelude_scripts(&self) -> &[(String, String)] {
        &self.prelude_scripts
    }

    /// Returns a reference to the extras map.
    #[inline]
    pub fn extras(&self) -> &Extras {
        &self.extras
    }

    /// Returns a mutable reference to the extras map.
    #[inline]
    pub fn extras_mut(&mut self) -> &mut Extras {
        &mut self.extras
    }

    /// Sets the pixel ratio (builder pattern).
    ///
    /// Invalid values (NaN, Inf, ≤0) are silently replaced with 1.0.
    ///
    /// # Arguments
    ///
    /// * `pixel_ratio` - Device pixel ratio (typically 1.0–4.0)
    #[must_use]
    pub fn with_pixel_ratio(mut self, pixel_ratio: f32) -> Self {
        // Defensive: avoid NaN/Inf/<=0 from JNI/JS inputs.
        self.pixel_ratio = if pixel_ratio.is_finite() && pixel_ratio > 0.0 {
            pixel_ratio
        } else {
            1.0
        };
        self
    }

    /// Sets the cache directory (builder pattern).
    ///
    /// # Arguments
    ///
    /// * `cache_dir` - Path to the cache directory
    #[must_use]
    pub fn with_cache_dir(mut self, cache_dir: PathBuf) -> Self {
        self.cache_dir = cache_dir;
        self
    }

    /// Sets the persistent files directory (builder pattern).
    ///
    /// # Arguments
    ///
    /// * `files_dir` - Path to the files directory
    #[must_use]
    pub fn with_files_dir(mut self, files_dir: PathBuf) -> Self {
        self.files_dir = files_dir;
        self
    }

    /// Alias for with_cache_dir for backward compatibility.
    #[must_use]
    #[deprecated(note = "Use with_cache_dir() instead")]
    pub fn with_tmp_dir(mut self, tmp_dir: PathBuf) -> Self {
        self.cache_dir = tmp_dir;
        self
    }

    /// Sets the code cache directory (builder pattern).
    ///
    /// # Arguments
    ///
    /// * `code_cache_dir` - Path to the code cache directory
    #[must_use]
    pub fn with_code_cache_dir(mut self, code_cache_dir: PathBuf) -> Self {
        self.code_cache_dir = code_cache_dir;
        self
    }

    /// Sets the target FPS (builder pattern).
    ///
    /// Values are clamped to the range [`crate::frame_rate`] serves.
    ///
    /// # Arguments
    ///
    /// * `fps` - Target frames per second
    #[must_use]
    pub fn with_target_fps(mut self, fps: i32) -> Self {
        self.target_fps = crate::frame_rate::clamp_fps(fps.max(0) as u32) as i32;
        self
    }

    /// Sets debug mode (builder pattern).
    ///
    /// # Arguments
    ///
    /// * `enabled` - Whether debug mode should be enabled
    #[must_use]
    pub fn with_debug_enabled(mut self, enabled: bool) -> Self {
        self.debug_enabled = enabled;
        self
    }

    /// Sets the log level (builder pattern).
    ///
    /// # Arguments
    ///
    /// * `level` - Log level
    #[must_use]
    pub fn with_log_level(mut self, level: LogLevel) -> Self {
        self.log_level = level;
        self
    }

    /// Sets the maximum memory limit (builder pattern).
    ///
    /// Values are clamped to the range [64, 2048].
    ///
    /// # Arguments
    ///
    /// * `mb` - Maximum memory in megabytes
    #[must_use]
    pub fn with_max_memory_mb(mut self, mb: i32) -> Self {
        self.max_memory_mb = mb.clamp(64, 2048);
        self
    }

    /// Enable or disable the ANR watchdog (builder pattern).
    ///
    /// When enabled, the process deadline scheduler guards synchronous V8
    /// execution and terminates an isolate that exceeds its budget.
    ///
    /// # Arguments
    ///
    /// * `enabled` - Whether the watchdog should be active
    #[must_use]
    pub fn with_watchdog_enabled(mut self, enabled: bool) -> Self {
        self.watchdog_enabled = enabled;
        self
    }

    /// Sets the ANR watchdog timeout in seconds (builder pattern).
    ///
    /// Values are clamped to the range [5, 120].
    ///
    /// # Arguments
    ///
    /// * `secs` - Timeout in seconds
    #[must_use]
    pub fn with_watchdog_timeout_secs(mut self, secs: i32) -> Self {
        self.watchdog_timeout_secs = secs.clamp(5, 120);
        self
    }

    /// Enable or disable code signing verification (builder pattern).
    ///
    /// When enabled, a valid Ed25519 public key must be provided via
    /// `with_code_signing_pubkey(...)`.
    #[must_use]
    pub fn with_code_signing_enabled(mut self, enabled: bool) -> Self {
        self.code_signing_enabled = enabled;
        self
    }

    /// Sets the Ed25519 public key for code signing verification (builder pattern).
    ///
    /// The key should be hex-encoded (64 characters for 32 bytes).
    /// Pass `None` to disable code signing verification.
    ///
    /// # Arguments
    ///
    /// * `pubkey` - Hex-encoded Ed25519 public key, or `None` to disable
    #[must_use]
    pub fn with_code_signing_pubkey(mut self, pubkey: Option<String>) -> Self {
        self.code_signing_pubkey = pubkey;
        self
    }

    /// Sets the subpackage definitions (builder pattern).
    #[must_use]
    pub fn with_sub_packages(mut self, packages: Vec<(String, String)>) -> Self {
        self.sub_packages = packages;
        self
    }

    /// Sets the workers path (builder pattern).
    #[must_use]
    pub fn with_workers_path(mut self, path: Option<String>) -> Self {
        self.workers_path = path;
        self
    }

    /// Appends a boot prelude script (builder pattern).
    ///
    /// The script runs as a plain (non-module) `<script>`-style snippet in
    /// the global scope before every `EvaluateModule`. Use this to inject
    /// a BOM/DOM adapter on top of the `migo.*` runtime so browser-style
    /// games (Cocos / Egret / Laya / Pixi / raw WebGL) load unchanged.
    ///
    /// `name` shows up in V8 stack traces. Multiple calls accumulate; the
    /// scripts execute in the order they were added.
    #[must_use]
    pub fn with_prelude_script(
        mut self,
        name: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        self.prelude_scripts.push((name.into(), source.into()));
        self
    }

    /// Replaces all boot prelude scripts (builder pattern).
    #[must_use]
    pub fn with_prelude_scripts(mut self, scripts: Vec<(String, String)>) -> Self {
        self.prelude_scripts = scripts;
        self
    }

    /// Adds a key-value pair to the extras map (builder pattern).
    ///
    /// # Arguments
    ///
    /// * `k` - Key name
    /// * `v` - Value (any type that converts to JSON Value)
    #[must_use]
    pub fn with_extra<V: Into<Value>>(mut self, k: impl Into<String>, v: V) -> Self {
        self.extras.insert(k.into(), v.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every declared variant round-trips through its own ordinal.
    ///
    /// Driven from the variant list rather than from six hand-written pairs:
    /// the numbers here are a copy of Java's `RuntimeConfig.LogLevel`, and a
    /// test that restated them would only prove the third copy agrees with
    /// itself.
    #[test]
    fn every_level_round_trips_through_its_ordinal() {
        for level in [
            LogLevel::Trace,
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Warn,
            LogLevel::Error,
            LogLevel::Off,
        ] {
            assert_eq!(LogLevel::from_ordinal(level as i32), Some(level));
        }
    }

    /// An ordinal outside the list is reported as unknown, not as a level.
    ///
    /// This is the state that used to be invisible: a host whose SDK declares a
    /// seventh level would have been read as Warn, silently, with the log at the
    /// level it did not ask for and nothing to say why.
    #[test]
    fn an_ordinal_the_two_enums_do_not_share_is_unknown() {
        assert_eq!(LogLevel::from_ordinal(6), None);
        assert_eq!(LogLevel::from_ordinal(-1), None);
        assert_eq!(LogLevel::from_ordinal(i32::MAX), None);
    }

    /// The lossy conversion still answers, because callers with nowhere to
    /// report still need a value. Pinned so the two entry points cannot drift
    /// into disagreeing about a shared ordinal.
    #[test]
    fn the_lossy_conversion_agrees_wherever_the_reporting_one_has_an_answer() {
        for ordinal in -2..8 {
            match LogLevel::from_ordinal(ordinal) {
                Some(level) => assert_eq!(LogLevel::from(ordinal), level),
                None => assert_eq!(LogLevel::from(ordinal), LogLevel::Warn),
            }
        }
    }

    #[test]
    fn prelude_scripts_default_empty() {
        let opt = InitOptions::new();
        assert!(opt.prelude_scripts().is_empty());
    }

    #[test]
    fn with_prelude_script_appends_in_order() {
        let opt = InitOptions::new()
            .with_prelude_script("<a>", "globalThis.a = 1;")
            .with_prelude_script("<b>", "globalThis.b = 2;");
        let scripts = opt.prelude_scripts();
        assert_eq!(scripts.len(), 2);
        assert_eq!(scripts[0].0, "<a>");
        assert_eq!(scripts[0].1, "globalThis.a = 1;");
        assert_eq!(scripts[1].0, "<b>");
        assert_eq!(scripts[1].1, "globalThis.b = 2;");
    }

    #[test]
    fn with_prelude_scripts_replaces_all() {
        let opt = InitOptions::new()
            .with_prelude_script("<old>", "noop")
            .with_prelude_scripts(vec![
                ("<x>".to_string(), "x".to_string()),
                ("<y>".to_string(), "y".to_string()),
            ]);
        let scripts = opt.prelude_scripts();
        assert_eq!(scripts.len(), 2);
        assert_eq!(scripts[0].0, "<x>");
        assert_eq!(scripts[1].0, "<y>");
    }

    /// Round-trip the JSON shape the Android JNI bridge produces (see
    /// `RuntimeConfig.buildPreludeScriptsJson`) — ensure the Rust side
    /// can deserialize it back into `(name, source)` pairs and that
    /// embedded control characters in `source` survive intact.
    ///
    /// Mirrors the parse_prelude_scripts implementation in
    /// `crates/platform/android/jni/inbound.rs`. Kept here so the
    /// contract is independently checkable on a host that can't build
    /// the Android NDK toolchain.
    #[test]
    fn prelude_scripts_json_roundtrip() {
        #[derive(serde::Deserialize)]
        struct Entry {
            name: String,
            source: String,
        }
        // What the Java side serializes for two entries, including a
        // newline + quote in the source to exercise escaping.
        let json =
            r#"[{"name":"<a>","source":"line1\nline2"},{"name":"<b>","source":"alert(\"hi\")"}]"#;
        let parsed: Vec<Entry> = serde_json::from_str(json).unwrap();
        let pairs: Vec<(String, String)> = parsed.into_iter().map(|e| (e.name, e.source)).collect();

        let opt = InitOptions::new().with_prelude_scripts(pairs);
        let s = opt.prelude_scripts();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].0, "<a>");
        assert_eq!(s[0].1, "line1\nline2");
        assert_eq!(s[1].0, "<b>");
        assert_eq!(s[1].1, "alert(\"hi\")");
    }

    /// A configured code-cache directory must be the one the runtime uses.
    ///
    /// The option was reachable from the C ABI and the Android bridge and had a
    /// getter, and `Host::new` still passed `cache_dir()` to the runtime, so
    /// nothing read it. See `docs/audits/2026-09-09/v8-core-shared.md:84-90`.
    #[test]
    fn a_configured_code_cache_dir_is_the_root_the_runtime_gets() {
        let options = InitOptions::new()
            .with_cache_dir(PathBuf::from("/tmp/ordinary-cache"))
            .with_code_cache_dir(PathBuf::from("/tmp/shared-code-cache"));
        assert_eq!(
            options.code_cache_root(),
            Path::new("/tmp/shared-code-cache")
        );
        assert_eq!(options.cache_dir(), Path::new("/tmp/ordinary-cache"));
    }

    /// An explicitly empty option falls back to the cache directory instead of
    /// resolving `migo_code_cache` under the process working directory. The
    /// default is not empty -- it is the same temp root as `cache_dir` -- so a
    /// host that never touched the option sees no change at all.
    #[test]
    fn an_empty_code_cache_dir_falls_back_to_the_cache_directory() {
        let options = InitOptions::new()
            .with_cache_dir(PathBuf::from("/tmp/ordinary-cache"))
            .with_code_cache_dir(PathBuf::new());
        assert_eq!(options.code_cache_root(), Path::new("/tmp/ordinary-cache"));

        let untouched = InitOptions::new().with_cache_dir(PathBuf::from("/tmp/ordinary-cache"));
        assert_eq!(untouched.code_cache_root(), untouched.code_cache_dir());
    }
}
