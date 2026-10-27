//! What `getAppBaseInfo`, `getAccountInfoSync` and `getDeviceBenchmarkInfo` answer -- and the parts of
//! `getSystemInfoSync` and `getDeviceInfo` that say the same things.
//!
//! Three sources, and nothing invented when one is silent: what the engine knows of itself (the API
//! level it implements, its own version, the content's id), what the host reports about itself and
//! the content it runs ([`HostAppInfo`]), and -- for the language, when the host does not name one --
//! the operating system's. A host that says nothing is answered with "unknown" where the platform has
//! a word for it (a benchmark level of `-1`, a model level of `0`) and the standard where it has a
//! standard (a text scale of `1`).

use serde::{Deserialize, Serialize};

/// The level of the common mini-game platform's API library this runtime implements, which content
/// reads as `SDKVersion` to decide which APIs it may call.
///
/// It is the newest level any API in the platform's reference was introduced at --
/// `library_level` in `contracts/runtime/minigame-api.json`, which the reference's refresh measures
/// from every API's own page -- and every one of those APIs the engine publishes, offers through
/// its host or declares unsupported (the API x platform matrix holds it to that). A test holds the
/// two together, so refreshing the reference and claiming its level are one change.
///
/// It used to be `"4.0.0"`: a level no release of the platform has, chosen to be above them all.
pub const API_LIBRARY_LEVEL: &str = "3.17.0";

/// This runtime's own version: `version`, which on the common platform is its client's version --
/// what runs the content, and what content works around a defect of by number.
pub const RUNTIME_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Whether this build runs on a desktop: the platform's PC client, whose kernel version it gives
/// content as `PCKernelVersion` -- "仅在 PC 端存在该值" -- and only there.
const DESKTOP: bool = cfg!(any(
    target_os = "macos",
    target_os = "windows",
    all(target_os = "linux", not(target_env = "ohos"))
));

/// The text size, in CSS pixels, that a text scale of 1 means: the web's `medium`.
const STANDARD_FONT_SIZE_PX: f64 = 16.0;

/// Which build of the content the host runs: the platform's `envVersion`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentEnvironment {
    /// A development build.
    Develop,
    /// A pre-release build handed to testers.
    Trial,
    /// The released build -- what a host that does not say runs.
    #[default]
    Release,
}

/// What the host says about itself and the content it runs. Every field is optional: a host that
/// says nothing gets the engine's own answer, never one made up for it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct HostAppInfo {
    /// The host app's own identifier -- its package name or bundle identifier: `host.appId`.
    pub host_app_id: Option<String>,
    /// The host app's language, as a BCP 47 tag (`zh-Hans-CN`) or a POSIX locale (`zh_CN.UTF-8`).
    pub language: Option<String>,
    /// The host app's text scale, `1` standard.
    pub font_scale: Option<f64>,
    /// Whether the host's debugging aids are on -- what `setEnableDebug` turns on and off.
    pub debug: bool,
    /// The host's own score for the device: `-1` unknown, otherwise `1` and up, higher faster.
    pub benchmark_level: Option<i32>,
    /// The host's tier for the device: `0` unknown, `1` high, `2` middle, `3` low.
    pub model_level: Option<u32>,
    /// Which build of the content runs.
    pub content_environment: ContentEnvironment,
    /// The content's released version, when it is a release.
    pub content_version: Option<String>,
}

/// The composed answer, as the op hands it to content's JavaScript.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    #[serde(rename = "SDKVersion")]
    pub sdk_version: &'static str,
    pub version: &'static str,
    /// The version of what runs the content on a desktop -- this engine, which is that kernel -- and
    /// absent elsewhere, as the platform gives it only on a PC.
    #[serde(rename = "PCKernelVersion", skip_serializing_if = "Option::is_none")]
    pub pc_kernel_version: Option<&'static str>,
    /// The content's id: the one identity the host gives it.
    pub app_id: String,
    pub host_app_id: Option<String>,
    /// `zh_CN`, `en_US`, `en`; empty when neither the host nor the system names one.
    pub language: String,
    pub font_size_scale_factor: f64,
    pub font_size_setting: u32,
    pub enable_debug: bool,
    pub benchmark_level: i32,
    pub model_level: u32,
    pub env_version: ContentEnvironment,
    /// Empty unless the content is a release whose version the host reported: the platform gives a
    /// development or trial build no version.
    pub content_version: String,
}

impl AppInfo {
    /// What content is told, from the host's report and the content's id.
    pub fn compose(host: HostAppInfo, app_id: &str) -> Self {
        let language = host
            .language
            .as_deref()
            .and_then(language_name)
            .or_else(|| os_language().as_deref().and_then(language_name))
            .unwrap_or_default();
        let scale = host
            .font_scale
            .filter(|scale| scale.is_finite() && *scale > 0.0)
            .unwrap_or(1.0);
        let content_version = match host.content_environment {
            ContentEnvironment::Release => host.content_version.unwrap_or_default(),
            ContentEnvironment::Develop | ContentEnvironment::Trial => String::new(),
        };
        Self {
            sdk_version: API_LIBRARY_LEVEL,
            version: RUNTIME_VERSION,
            pc_kernel_version: DESKTOP.then_some(RUNTIME_VERSION),
            app_id: app_id.to_owned(),
            host_app_id: host.host_app_id.filter(|id| !id.is_empty()),
            language,
            font_size_scale_factor: scale,
            font_size_setting: (STANDARD_FONT_SIZE_PX * scale).round() as u32,
            enable_debug: host.debug,
            benchmark_level: host.benchmark_level.unwrap_or(-1),
            model_level: host.model_level.filter(|level| *level <= 3).unwrap_or(0),
            env_version: host.content_environment,
            content_version,
        }
    }
}

/// A language as content is told it: the language subtag, then the region if there is one --
/// `zh_CN`, `en_US`, `en` -- from a BCP 47 tag (`zh-Hans-CN`) or a POSIX locale
/// (`zh_CN.UTF-8@euro`). The script is dropped: the region already tells the variants content
/// branches on apart, and the platform's own names carry none. `None` for the POSIX "C" locale,
/// which names no language, and for anything that does not begin with one.
pub fn language_name(tag: &str) -> Option<String> {
    let tag = tag.split(['.', '@']).next().unwrap_or("");
    let mut subtags = tag.split(['-', '_']);
    let language = subtags.next()?;
    if !(2..=3).contains(&language.len()) || !language.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let mut name = language.to_ascii_lowercase();
    for subtag in subtags {
        let region = (subtag.len() == 2 && subtag.bytes().all(|b| b.is_ascii_alphabetic()))
            || (subtag.len() == 3 && subtag.bytes().all(|b| b.is_ascii_digit()));
        if region {
            name.push('_');
            name.push_str(&subtag.to_ascii_uppercase());
            break;
        }
        // A script (`Hans`) sits between the language and the region; anything else ends it.
        if !(subtag.len() == 4 && subtag.bytes().all(|b| b.is_ascii_alphabetic())) {
            break;
        }
    }
    Some(name)
}

/// The operating system's user interface language, for a host that names none, as the system
/// gives it; `None` where the engine cannot read it -- Android and OpenHarmony, whose host layers
/// report it.
pub fn os_language() -> Option<String> {
    os::language()
}

#[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
mod os {
    /// gettext's order: the locale for messages is the first of `LC_ALL`, `LC_MESSAGES` and `LANG`
    /// that is set; `LANGUAGE`, a preference list, overrides it unless that locale is "C", which
    /// switches translation off.
    pub(super) fn language() -> Option<String> {
        from_environment(|name| std::env::var(name).ok())
    }

    pub(super) fn from_environment(var: impl Fn(&str) -> Option<String>) -> Option<String> {
        let set = |name: &str| var(name).filter(|value| !value.is_empty());
        let locale = set("LC_ALL")
            .or_else(|| set("LC_MESSAGES"))
            .or_else(|| set("LANG"))?;
        if locale == "C" || locale == "POSIX" || locale.starts_with("C.") {
            return None;
        }
        let preferred = set("LANGUAGE").and_then(|list| {
            list.split(':')
                .find(|entry| !entry.is_empty())
                .map(str::to_owned)
        });
        Some(preferred.unwrap_or(locale))
    }
}

#[cfg(windows)]
mod os {
    use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};

    /// The first of the user's preferred display languages, a BCP 47 name.
    pub(super) fn language() -> Option<String> {
        let mut count = 0u32;
        let mut length = 0u32;
        // SAFETY: a null buffer with a zero length asks for the length needed.
        let sized = unsafe {
            GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &mut count,
                std::ptr::null_mut(),
                &mut length,
            )
        };
        if sized == 0 || length == 0 {
            return None;
        }
        let mut buffer = vec![0u16; length as usize];
        // SAFETY: `buffer` holds the `length` UTF-16 units the first call asked for.
        let filled = unsafe {
            GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &mut count,
                buffer.as_mut_ptr(),
                &mut length,
            )
        };
        if filled == 0 {
            return None;
        }
        // A double-NUL-terminated list; the first entry is the preferred language.
        let first = buffer.split(|unit| *unit == 0).next()?;
        (!first.is_empty()).then(|| String::from_utf16_lossy(first))
    }
}

#[cfg(target_vendor = "apple")]
mod os {
    use std::ffi::{c_char, c_void};

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFLocaleCopyPreferredLanguages() -> *const c_void;
        fn CFArrayGetCount(array: *const c_void) -> isize;
        fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
        fn CFStringGetCString(
            string: *const c_void,
            buffer: *mut c_char,
            size: isize,
            encoding: u32,
        ) -> u8;
        fn CFRelease(cf: *const c_void);
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    /// The first of the user's preferred languages, a BCP 47 tag.
    pub(super) fn language() -> Option<String> {
        // SAFETY: a Copy-rule function; the array it returns is released below.
        let languages = unsafe { CFLocaleCopyPreferredLanguages() };
        if languages.is_null() {
            return None;
        }
        // SAFETY: `languages` is a live CFArray of CFStrings until the release, and the buffer is
        // as large as the size passed; a tag that does not fit fails the copy rather than truncating.
        let first = unsafe {
            let mut buffer = [0 as c_char; 64];
            let tag = (CFArrayGetCount(languages) > 0)
                .then(|| CFArrayGetValueAtIndex(languages, 0))
                .filter(|tag| !tag.is_null())
                .filter(|tag| {
                    CFStringGetCString(
                        *tag,
                        buffer.as_mut_ptr(),
                        buffer.len() as isize,
                        K_CF_STRING_ENCODING_UTF8,
                    ) != 0
                })
                .and_then(|_| std::ffi::CStr::from_ptr(buffer.as_ptr()).to_str().ok())
                .map(str::to_owned);
            CFRelease(languages);
            tag
        };
        first.filter(|tag| !tag.is_empty())
    }
}

#[cfg(not(any(unix, windows)))]
mod os {
    pub(super) fn language() -> Option<String> {
        None
    }
}

#[cfg(target_os = "android")]
mod os {
    /// The process environment says nothing about the user's language on Android; the Java layer
    /// reports the app's.
    pub(super) fn language() -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The level the engine claims is the one the reference measures: refreshing the reference
    /// (scripts/fetch-minigame-api-reference.py) and claiming its level are one change.
    #[test]
    fn the_claimed_level_is_the_reference_level() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../contracts/runtime/minigame-api.json"
        );
        let reference: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("the API reference"))
                .unwrap();
        assert_eq!(
            reference["library_level"]["version"].as_str(),
            Some(API_LIBRARY_LEVEL),
            "{path} measures a different level"
        );
    }

    #[test]
    fn a_language_is_named_by_its_language_and_region() {
        for (tag, name) in [
            ("zh-Hans-CN", Some("zh_CN")),
            ("zh-Hant-TW", Some("zh_TW")),
            ("zh_CN.UTF-8", Some("zh_CN")),
            ("de_DE.UTF-8@euro", Some("de_DE")),
            ("en-US", Some("en_US")),
            ("EN-us", Some("en_US")),
            ("en", Some("en")),
            ("zh-Hans", Some("zh")),
            ("es-419", Some("es_419")),
            ("fil-PH", Some("fil_PH")),
            ("sr-Latn-RS-x-private", Some("sr_RS")),
            ("en-GB-oxendict", Some("en_GB")),
            ("C", None),
            ("POSIX", None),
            ("", None),
            ("1x-US", None),
            ("english", None),
        ] {
            assert_eq!(language_name(tag).as_deref(), name, "{tag}");
        }
    }

    #[test]
    fn a_host_that_says_nothing_is_told_unknown_or_standard_never_an_invention() {
        let info = AppInfo::compose(HostAppInfo::default(), "demo");
        assert_eq!(info.sdk_version, API_LIBRARY_LEVEL);
        assert_eq!(info.version, RUNTIME_VERSION);
        assert_eq!(info.app_id, "demo");
        assert_eq!(info.host_app_id, None);
        assert_eq!(info.font_size_scale_factor, 1.0);
        assert_eq!(info.font_size_setting, 16);
        assert!(!info.enable_debug);
        assert_eq!(info.benchmark_level, -1);
        assert_eq!(info.model_level, 0);
        assert_eq!(info.env_version, ContentEnvironment::Release);
        assert_eq!(info.content_version, "");
        // Whatever the system says, or nothing; never a language the engine picked.
        assert_eq!(
            info.language,
            os_language()
                .as_deref()
                .and_then(language_name)
                .unwrap_or_default()
        );
    }

    #[test]
    fn what_the_host_reports_is_what_content_is_told() {
        let info = AppInfo::compose(
            HostAppInfo {
                host_app_id: Some("com.example.host".into()),
                language: Some("zh-Hans-CN".into()),
                font_scale: Some(1.3),
                debug: true,
                benchmark_level: Some(31),
                model_level: Some(2),
                content_environment: ContentEnvironment::Release,
                content_version: Some("1.4.2".into()),
            },
            "demo",
        );
        assert_eq!(info.host_app_id.as_deref(), Some("com.example.host"));
        assert_eq!(info.language, "zh_CN");
        assert_eq!(info.font_size_scale_factor, 1.3);
        assert_eq!(info.font_size_setting, 21);
        assert!(info.enable_debug);
        assert_eq!(info.benchmark_level, 31);
        assert_eq!(info.model_level, 2);
        assert_eq!(info.content_version, "1.4.2");
    }

    #[test]
    fn a_build_that_is_not_a_release_has_no_version() {
        for environment in [ContentEnvironment::Develop, ContentEnvironment::Trial] {
            let info = AppInfo::compose(
                HostAppInfo {
                    content_environment: environment,
                    content_version: Some("1.4.2".into()),
                    ..HostAppInfo::default()
                },
                "demo",
            );
            assert_eq!(info.env_version, environment);
            assert_eq!(info.content_version, "");
        }
    }

    #[test]
    fn values_outside_their_range_are_unknown() {
        let info = AppInfo::compose(
            HostAppInfo {
                host_app_id: Some(String::new()),
                font_scale: Some(f64::NAN),
                model_level: Some(7),
                ..HostAppInfo::default()
            },
            "demo",
        );
        assert_eq!(info.host_app_id, None);
        assert_eq!(info.font_size_scale_factor, 1.0);
        assert_eq!(info.model_level, 0);
    }

    #[test]
    fn the_composed_answer_has_the_names_content_reads() {
        let json = serde_json::to_value(AppInfo::compose(HostAppInfo::default(), "demo")).unwrap();
        for field in [
            "SDKVersion",
            "version",
            "appId",
            "hostAppId",
            "language",
            "fontSizeScaleFactor",
            "fontSizeSetting",
            "enableDebug",
            "benchmarkLevel",
            "modelLevel",
            "envVersion",
            "contentVersion",
        ] {
            assert!(json.get(field).is_some(), "missing {field}: {json}");
        }
        assert_eq!(json["envVersion"], "release");
    }

    #[test]
    fn a_host_report_reads_from_its_json() {
        let host: HostAppInfo = serde_json::from_str(
            r#"{"hostAppId":"com.example","language":"en-US","fontScale":1.15,"debug":true,
                "benchmarkLevel":-1,"modelLevel":0,"contentEnvironment":"trial","contentVersion":""}"#,
        )
        .unwrap();
        assert_eq!(host.content_environment, ContentEnvironment::Trial);
        assert_eq!(host.font_scale, Some(1.15));
        // And a report that leaves everything out is the default.
        assert_eq!(
            serde_json::from_str::<HostAppInfo>("{}").unwrap(),
            HostAppInfo::default()
        );
    }

    /// Where the engine reads the system's language itself, a user always has one.
    #[cfg(any(target_vendor = "apple", windows))]
    #[test]
    fn the_system_names_a_language() {
        let tag = os_language().expect("the system's preferred language");
        assert!(language_name(&tag).is_some(), "{tag}");
    }

    #[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
    #[test]
    fn the_environment_names_the_language_as_gettext_reads_it() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        assert_eq!(
            os::from_environment(env(&[("LANG", "de_DE.UTF-8")])).as_deref(),
            Some("de_DE.UTF-8")
        );
        assert_eq!(
            os::from_environment(env(&[
                ("LANG", "de_DE.UTF-8"),
                ("LC_MESSAGES", "fr_FR.UTF-8")
            ]))
            .as_deref(),
            Some("fr_FR.UTF-8")
        );
        assert_eq!(
            os::from_environment(env(&[("LANG", "de_DE.UTF-8"), ("LC_ALL", "ja_JP.UTF-8")]))
                .as_deref(),
            Some("ja_JP.UTF-8")
        );
        assert_eq!(
            os::from_environment(env(&[("LANG", "en_US.UTF-8"), ("LANGUAGE", ":pt_BR:en")]))
                .as_deref(),
            Some("pt_BR")
        );
        // "C" switches translation off, LANGUAGE included; nothing set is nothing known.
        assert_eq!(
            os::from_environment(env(&[("LANG", "C.UTF-8"), ("LANGUAGE", "fr")])),
            None
        );
        assert_eq!(os::from_environment(env(&[("LANG", "")])), None);
        assert_eq!(os::from_environment(env(&[])), None);
    }
}
