//! The faces the system's fontconfig gives the CSS generic family keywords, on Linux.
//!
//! Skia's fontconfig font manager does not answer a generic keyword itself. It accepts a match only when the face
//! fontconfig returns carries one of the *strongly* bound family names of the substituted pattern, and a system's
//! configuration binds the faces it prefers for `serif`, `sans-serif` or `monospace` weakly (its `<alias>`es), so
//! `matchFamilyStyle("serif")` answers nothing on an ordinary install. Measured on Ubuntu 24.04 with DejaVu: the
//! manager lists DejaVu Sans, DejaVu Serif and DejaVu Sans Mono and matches each by name, and answers none of the
//! keywords. Every keyword fell through the text chain to the bundled Noto Sans: a `monospace` label came out in a
//! proportional face, and `serif` was the same face as `sans-serif`.
//!
//! A browser on Linux asks fontconfig what a keyword means on this system. This does the same, through fontconfig's
//! own resolution of a font request: the configured substitutions (`FcConfigSubstitute`, where the system's and the
//! user's preferences for each keyword live), the defaults (`FcDefaultSubstitute`), then the best match
//! (`FcFontMatch`). The family of that match is the face; the text chain puts it before the keyword. A system with
//! no fonts at all answers nothing, and the chain falls through as before.
//!
//! The keywords are the ones browsers on Linux resolve: CSS 2's five generics and `system-ui`. The `ui-*` keywords
//! name a platform's own UI faces, which only Apple's systems define, and are left unmatched here as the
//! specification says.
//!
//! Each keyword is resolved once per process ([`resolve`] at `TextContext` construction, off the critical path):
//! the answer only changes when the system's fonts or configuration do, which a running game does not see either
//! in a browser.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;
use std::sync::OnceLock;

#[repr(C)]
struct FcPattern {
    _opaque: [u8; 0],
}

type FcBool = c_int;
type FcResult = c_int;

/// `FcResultMatch`.
const FC_RESULT_MATCH: FcResult = 0;
/// `FcMatchPattern`: substitute a pattern before matching, as a font request is.
const FC_MATCH_PATTERN: c_int = 0;
/// `FC_FAMILY`.
const FC_FAMILY: &CStr = c"family";

// libfontconfig is already a link dependency of Skia on this platform; naming it here is what lets these
// declarations stand on their own.
#[link(name = "fontconfig")]
unsafe extern "C" {
    fn FcPatternCreate() -> *mut FcPattern;
    fn FcPatternDestroy(pattern: *mut FcPattern);
    fn FcPatternAddString(
        pattern: *mut FcPattern,
        object: *const c_char,
        value: *const u8,
    ) -> FcBool;
    fn FcPatternGetString(
        pattern: *const FcPattern,
        object: *const c_char,
        index: c_int,
        value: *mut *mut u8,
    ) -> FcResult;
    fn FcConfigSubstitute(config: *mut c_void, pattern: *mut FcPattern, kind: c_int) -> FcBool;
    fn FcDefaultSubstitute(pattern: *mut FcPattern);
    fn FcFontMatch(
        config: *mut c_void,
        pattern: *mut FcPattern,
        result: *mut FcResult,
    ) -> *mut FcPattern;
}

/// The keywords resolved, in the name fontconfig knows each by.
const KEYWORDS: [&CStr; 6] = [
    c"serif",
    c"sans-serif",
    c"monospace",
    c"cursive",
    c"fantasy",
    c"system-ui",
];

/// A pattern this module owns, destroyed when it goes out of scope.
struct Pattern(*mut FcPattern);

impl Drop for Pattern {
    fn drop(&mut self) {
        // SAFETY: the pointer came from fontconfig, is non-null (checked where the value is made) and is destroyed
        // only here.
        unsafe { FcPatternDestroy(self.0) }
    }
}

/// The family fontconfig gives a request for `keyword`, or `None` when it gives no font at all.
fn query(keyword: &CStr) -> Option<String> {
    // SAFETY: every pointer passed is either one fontconfig returned and this function still owns (checked non-null
    // first), a NUL-terminated literal, or null for the current configuration, which fontconfig documents for every
    // `config` parameter used here. The family string belongs to `matched` and is copied before it is destroyed.
    unsafe {
        let request = FcPatternCreate();
        if request.is_null() {
            return None;
        }
        let request = Pattern(request);
        if FcPatternAddString(request.0, FC_FAMILY.as_ptr(), keyword.as_ptr().cast()) == 0
            || FcConfigSubstitute(ptr::null_mut(), request.0, FC_MATCH_PATTERN) == 0
        {
            return None;
        }
        FcDefaultSubstitute(request.0);
        let mut result: FcResult = 0;
        let matched = FcFontMatch(ptr::null_mut(), request.0, &mut result);
        if matched.is_null() {
            return None;
        }
        let matched = Pattern(matched);
        let mut family: *mut u8 = ptr::null_mut();
        if FcPatternGetString(matched.0, FC_FAMILY.as_ptr(), 0, &mut family) != FC_RESULT_MATCH
            || family.is_null()
        {
            return None;
        }
        CStr::from_ptr(family.cast())
            .to_str()
            .ok()
            .map(str::to_owned)
    }
}

fn resolved() -> &'static [Option<String>; KEYWORDS.len()] {
    static RESOLVED: OnceLock<[Option<String>; KEYWORDS.len()]> = OnceLock::new();
    RESOLVED.get_or_init(|| KEYWORDS.map(query))
}

/// Resolve every keyword now, so the first text drawn does not pay for it.
pub(crate) fn resolve() {
    resolved();
}

/// The face fontconfig gives the CSS generic keyword `family`, or `None` when `family` is not such a keyword or
/// fontconfig gives it no font.
pub(crate) fn generic_face(family: &str) -> Option<&'static str> {
    let index = KEYWORDS
        .iter()
        .position(|keyword| keyword.to_bytes().eq_ignore_ascii_case(family.as_bytes()))?;
    resolved()[index].as_deref()
}
