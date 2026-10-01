//! `Intl`, `toLocaleString`, `localeCompare` and `normalize` work.
//!
//! V8 does not carry its own locale data: the embedder hands it ICU's common data before the first isolate, or every
//! locale-sensitive built-in fails. `deno_core` does that when its `include_icu_data` feature is on, and this runtime
//! built it with default features off -- so `new Intl.NumberFormat('en-US')` threw `TypeError: Internal error. Icu
//! error.`, `new Intl.DateTimeFormat()` and `new Intl.Segmenter()` aborted the process (a V8 CHECK, not an exception),
//! and nothing noticed: no test had ever formatted a number. Found running Pixi, whose text layout builds an
//! `Intl.Segmenter`. A game that writes `score.toLocaleString()` is one line from the same fate.
//!
//! The assertions are what any browser answers for `en-US`; they do not depend on the machine's locale.

#[cfg(test)]
mod intl_tests {
    use deno_core::{FastString, JsRuntime, RuntimeOptions};

    fn eval_bool(source: &str) {
        // A bare `JsRuntime` here, not the engine's: it gets the data the way the engine does, first.
        crate::install_icu_data();
        let mut runtime = JsRuntime::new(RuntimeOptions::default());
        let script = format!(
            "(() => {{ const r = (() => {{ {source} }})(); if (r !== true) throw new Error('got ' + String(r)); }})()"
        );
        runtime
            .execute_script("<test:intl>", FastString::from(script))
            .unwrap_or_else(|error| panic!("{source}\n{error}"));
    }

    #[test]
    fn numbers_format_with_grouping() {
        eval_bool("return new Intl.NumberFormat('en-US').format(1234567.891) === '1,234,567.891';");
        eval_bool("return (1234.5).toLocaleString('en-US') === '1,234.5';");
        eval_bool(
            "return new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' }).format(3.5) === '$3.50';",
        );
    }

    #[test]
    fn dates_format_in_a_named_zone() {
        eval_bool(
            "return new Intl.DateTimeFormat('en-US', { timeZone: 'UTC' }).format(new Date(0)) === '1/1/1970';",
        );
        eval_bool(
            "return new Date(0).toLocaleDateString('en-US', { timeZone: 'UTC' }) === '1/1/1970';",
        );
    }

    #[test]
    fn strings_compare_and_normalise() {
        eval_bool("return 'a'.localeCompare('b') < 0 && 'b'.localeCompare('a') > 0;");
        eval_bool(
            "return ['b', 'a', 'C'].sort(new Intl.Collator('en').compare).join() === 'a,b,C';",
        );
        eval_bool("return 'e\\u0301'.normalize('NFC').length === 1;");
        eval_bool("return 'I'.toLocaleLowerCase('tr') === '\\u0131';");
    }

    #[test]
    fn the_other_intl_constructors_work() {
        eval_bool("return new Intl.PluralRules('en').select(1) === 'one';");
        eval_bool("return new Intl.RelativeTimeFormat('en').format(1, 'day') === 'in 1 day';");
        eval_bool("return new Intl.ListFormat('en').format(['a', 'b', 'c']) === 'a, b, and c';");
        eval_bool(
            "return new Intl.DisplayNames(['en'], { type: 'region' }).of('US') === 'United States';",
        );
    }

    /// What the filtered data (see `migo-icu-data`'s policy) is kept FOR, one assertion per part of it: dropping an item
    /// the policy should have kept shows up here, not in a game.
    #[test]
    fn each_part_of_the_filtered_data_still_does_its_job() {
        // The core data of every locale, kept for all of them: a German player's number is not an English one.
        eval_bool("return new Intl.NumberFormat('de-DE').format(1234.5) === '1.234,5';");
        eval_bool("return new Intl.NumberFormat('hi-IN').format(1234567) === '12,34,567';");
        eval_bool("return new Intl.NumberFormat('sw-KE').format(1234.5) === '1,234.5';");
        eval_bool(
            "return new Intl.DateTimeFormat('de-DE', { timeZone: 'UTC', dateStyle: 'long' }).format(new Date(0)) === '1. Januar 1970';",
        );
        eval_bool(
            "return new Intl.DateTimeFormat('ja-JP', { timeZone: 'UTC', dateStyle: 'long' }).format(new Date(0)) === '1970\\u5e741\\u67081\\u65e5';",
        );
        // Time zone rules: Shanghai is eight hours from UTC, whatever the machine's own zone.
        eval_bool(
            "return new Date(0).toLocaleString('en-US', { timeZone: 'Asia/Shanghai' }) === '1/1/1970, 8:00:00 AM';",
        );
        eval_bool(
            "return new Intl.DateTimeFormat('en-US', { timeZone: 'America/New_York', timeZoneName: 'long' }).format(new Date(0)).endsWith('Eastern Standard Time');",
        );
        // Collation for Chinese: pinyin order sorts the players of a Chinese leaderboard.
        eval_bool(
            "return ['\\u738b', '\\u5f20', '\\u674e'].sort(new Intl.Collator('zh-u-co-pinyin').compare).join('') === '\\u674e\\u738b\\u5f20';",
        );
        // Display names for a kept language, and the root's spelling for one that was dropped.
        eval_bool(
            "return new Intl.NumberFormat('en-US', { style: 'currency', currency: 'EUR', currencyDisplay: 'name' }).format(2) === '2.00 euros';",
        );
        eval_bool(
            "return new Intl.DisplayNames(['zh'], { type: 'region' }).of('US') === '\\u7f8e\\u56fd';",
        );
        eval_bool(
            "return new Intl.NumberFormat('sw-KE', { style: 'currency', currency: 'KES' }).format(1) !== '';",
        );
        // Chinese word segmentation needs the dictionary (cjdict) the policy keeps: without it a run of Han characters is one
        // "word", with it the sentence is words (我 / 爱 / 北京 / 天安门).
        eval_bool(
            "const s = new Intl.Segmenter('zh', { granularity: 'word' }); \
             const words = Array.from(s.segment('\\u6211\\u7231\\u5317\\u4eac\\u5929\\u5b89\\u95e8')).filter((x) => x.isWordLike); \
             return words.length >= 3 && words.length < 7;",
        );
    }

    /// Text layout libraries (Pixi, many others) split text into user-perceived characters with it.
    #[test]
    fn a_segmenter_splits_into_graphemes_and_words() {
        eval_bool(
            "const s = new Intl.Segmenter('en', { granularity: 'grapheme' }); \
             return Array.from(s.segment('a\\u0301bc')).length === 3;",
        );
        eval_bool(
            "const s = new Intl.Segmenter('en', { granularity: 'word' }); \
             return Array.from(s.segment('hello world')).filter((x) => x.isWordLike).length === 2;",
        );
    }
}
