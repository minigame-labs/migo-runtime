//! V8's locale data.
//!
//! V8 does not carry its own: the embedder hands it ICU's common data before the first isolate is created, and until it
//! has, `Intl.NumberFormat` throws `Internal error. Icu error.`, `toLocaleString` and `localeCompare` fail with it, and
//! `new Intl.DateTimeFormat()` or `new Intl.Segmenter()` aborts the whole process (a V8 `CHECK`, not an exception).
//! `deno_core` does this itself when its `include_icu_data` feature is on, and this runtime builds it without; the data
//! it would install is the full 10.7 MB file, so the data here is `migo-icu-data`'s filtered package instead (see its
//! `policy`).
//!
//! Once per process, before the first runtime of any kind -- the main one, a prewarmed one on a background thread, a
//! worker's: V8 initialises its platform on the first isolate and the data cannot be handed over after that.

use std::sync::Once;

static INSTALLED: Once = Once::new();

/// Hand V8 its ICU data. Idempotent; concurrent callers wait for the first. A failure is logged, not fatal: the engine
/// runs, `Intl` does not (and the `intl` tests are what keep that from shipping).
pub fn install_icu_data() {
    INSTALLED.call_once(|| {
        if let Err(code) = deno_core::v8::icu::set_common_data_77(icu_data::ICU_DATA) {
            tracing::error!(
                "V8 refused the ICU data (ICU error {code}): Intl, toLocaleString and localeCompare will fail"
            );
        }
    });
}
