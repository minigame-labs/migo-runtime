//! What of ICU's data a game engine ships, and why.
//!
//! V8's `Intl`, `toLocaleString`, `localeCompare` and `normalize` read ICU's common data; without it they throw
//! `Icu error` or abort the process (see `runtime-v8`'s `intl` tests). The whole file is 10.7 MB, which is more than a
//! quarter of the Android library, and most of it is data no mini-game reads: character-set converters V8 never opens,
//! transliteration and spell-out rules that are not in ECMA-402, and localised *display names* (languages, regions,
//! currencies, units, time zones) for 650 locales.
//!
//! What stays is chosen by what is wrong when it is missing:
//!
//!   * **The core data of every locale** (number and date patterns, month and day names, plural rules). Without a
//!     locale's file ICU falls back to the root, which formats `1234.5` as `1,234.5` for a German user: a wrong
//!     answer, not a missing one. The files are small (about 2 KB each), so all of them stay.
//!   * **Break iterators and their dictionaries**: `Intl.Segmenter`, line breaking in text layout, Chinese and Japanese
//!     word segmentation (`cjdict`, 2 MB). Without `cjdict` nothing throws: `我们是中国人` comes back as one word, the
//!     same kind of wrong answer the rule above refuses.
//!   * **Collation** for the root and for Chinese (pinyin, stroke and zhuyin orders: sorting a Chinese player list).
//!     Without the tailoring Chinese sorts by code point, and `'张'.localeCompare('李', 'zh')` changes sign.
//!   * **Time zones**: the rules (`zoneinfo64`, `metaZones`, `windowsZones`) are part of the core and stay.
//!   * **Display names** (languages, regions, currencies, units, zone names) only for the locales below: the top
//!     languages of the markets this engine ships to, South Asia (`hi`, `bn`, `ur`) and South-East Asia (`ms`, `fil`)
//!     among them. Everything else falls back to English, the default locale: a Hindi currency name read
//!     "Indian rupees" before `hi` was listed. About 40 KB a language.
//!
//! Dropped: converters, transliteration, number spell-out, the confusables and UTS #46 tables, the "scf" normalizer,
//! and the collation tailorings and display names of the locales not listed.

/// The locales whose display names, collation and units are kept. `root` and the shared `pool` / `res_index` bundles
/// are always kept: other items refer to them.
pub const KEEP_LOCALES: &[&str] = &[
    "root", "en", "zh", "ja", "ko", "es", "fr", "de", "pt", "ru", "ar", "id", "th", "vi", "tr",
    "it", "hi", "bn", "ur", "ms", "fil",
];

/// Collation tailorings kept besides the root's.
pub const KEEP_COLLATION: &[&str] = &["zh"];

const ALWAYS: &[&str] = &["pool", "res_index"];

/// The language of a locale file stem: `zh_Hant_TW` -> `zh`.
fn language(stem: &str) -> &str {
    stem.split(['_', '-']).next().unwrap_or(stem)
}

/// Whether an item is shipped. `name` is as the table of contents spells it, `icudt77l/zone/en.res`.
pub fn keep(name: &str) -> bool {
    // The leading `icudt77l/` is the package prefix, not part of the item.
    let item = name.split_once('/').map_or(name, |(_, rest)| rest);

    // Not in ECMA-402, or never opened by V8.
    if item.ends_with(".cnv")
        || matches!(
            item,
            "cnvalias.icu" | "confusables.cfu" | "uts46.nrm" | "nfkc_scf.nrm"
        )
        || item.starts_with("translit/")
        || item.starts_with("rbnf/")
    {
        return false;
    }

    let Some((dir, file)) = item.split_once('/') else {
        // A top-level item: the core locale files, the normalizer tables, the property tables, time zones.
        return true;
    };
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    match dir {
        "lang" | "region" | "curr" | "unit" | "zone" => {
            ALWAYS.contains(&stem) || KEEP_LOCALES.contains(&language(stem))
        }
        "coll" => {
            ALWAYS.contains(&stem)
                || stem == "root"
                || stem == "ucadata"
                || KEEP_COLLATION.contains(&language(stem))
        }
        // Break iterators and dictionaries: every one stays. A missing dictionary is not an error but a silent
        // wrong answer -- the script's run is treated as one word (see the module docs).
        _ => true,
    }
}
