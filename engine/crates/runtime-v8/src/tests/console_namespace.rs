//! The console has every member of the Console Standard's namespace, and each writes what a
//! browser's would.
//!
//! It had seven: debug, log, info, warn, error, group, groupEnd -- the seven the mini-game
//! reference lists. Content calls the others because a browser has them, and a member that is not
//! there is a TypeError: Cocos Creator 3 calls `console.time` while it starts its physics, and its
//! start-up ended there.
//!
//! Each check reads the lines back from the session's console buffer, the one the host's on-screen
//! console shows, so it is the text a developer would see.

#[cfg(test)]
mod console_namespace_tests {
    use deno_core::FastString;

    use super::super::support::{boot, test_host_state};

    struct Session(i32);

    impl Drop for Session {
        fn drop(&mut self) {
            shared::console_log::unregister_console_log(self.0);
            crate::console::bind_thread_console(self.0);
        }
    }

    /// The (level, message) lines `source` writes. Levels: 0 debug, 1 log, 2 warn, 3 error.
    fn lines(host_id: i32, source: &'static str) -> Vec<(u8, String)> {
        let buffer = shared::console_log::register_console_log(host_id, true, None);
        let _session = Session(host_id);
        crate::console::bind_thread_console(host_id);
        let mut runtime = boot(test_host_state());
        runtime
            .execute_script("<test:console>", FastString::from_static(source))
            .expect("the console calls");
        let (entries, _) = buffer.read_since(0);
        entries.into_iter().map(|e| (e.level, e.message)).collect()
    }

    #[test]
    fn every_member_of_the_namespace_is_a_function() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        let found = lines(
            9_501,
            r#"console.log(["assert", "clear", "count", "countReset", "debug", "dir", "dirxml", "error", "group",
                "groupCollapsed", "groupEnd", "info", "log", "table", "time", "timeEnd", "timeLog", "trace", "warn"]
                .filter(function (name) { return typeof console[name] !== "function"; }).join(",") || "all");"#,
        );
        assert_eq!(found, vec![(1, "all".to_string())]);
    }

    #[test]
    fn assert_writes_an_error_only_when_it_fails() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        let found = lines(
            9_502,
            r#"console.assert(true, "never"); console.assert(false); console.assert(0, "lost", 3);
               console.assert(false, { at: 1 });"#,
        );
        assert_eq!(
            found,
            vec![
                (3, "Assertion failed".to_string()),
                (3, "Assertion failed: lost 3".to_string()),
                (3, "Assertion failed: {\"at\":1}".to_string()),
            ]
        );
    }

    #[test]
    fn count_counts_per_label_and_resets() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        let found = lines(
            9_503,
            r#"console.count(); console.count(); console.count("a"); console.countReset();
               console.count(); console.countReset("never");"#,
        );
        assert_eq!(
            found,
            vec![
                (1, "default: 1".to_string()),
                (1, "default: 2".to_string()),
                (1, "a: 1".to_string()),
                (1, "default: 1".to_string()),
                (2, "Count for 'never' does not exist".to_string()),
            ]
        );
    }

    #[test]
    fn timers_report_their_label_and_end_once() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        let found = lines(
            9_504,
            r#"console.time("t"); console.time("t"); console.timeLog("t", "midway"); console.timeEnd("t");
               console.timeEnd("t"); console.timeLog("none");"#,
        );
        assert_eq!(found.len(), 5, "{found:?}");
        assert_eq!(found[0], (2, "Timer 't' already exists".to_string()));
        assert!(
            found[1].0 == 1 && found[1].1.starts_with("t: ") && found[1].1.ends_with("ms midway"),
            "{found:?}"
        );
        assert!(
            found[2].0 == 1 && found[2].1.starts_with("t: ") && found[2].1.ends_with("ms"),
            "{found:?}"
        );
        assert_eq!(found[3], (2, "Timer 't' does not exist".to_string()));
        assert_eq!(found[4], (2, "Timer 'none' does not exist".to_string()));
    }

    #[test]
    fn a_string_first_argument_is_formatted_as_the_standard_says() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        let found = lines(
            9_506,
            r#"console.log("%s is %d years, %f%%", "Ann", "42.9", "1.5");
               console.log("%o and %O", { a: 1 }, [1]);
               console.log("%c styled", "color: red");
               console.log("%d %i", "x", Symbol("s"));
               console.log("%s", "a", "b", { c: 2 });
               console.log("100%");
               console.log(1, "%s");
               console.log("%s", { toString: function () { throw new Error("no"); } });
               console.assert(false, "value=%d", 3);
               console.time("%s"); console.timeLog("%s", "kept");"#,
        );
        let texts: Vec<&str> = found.iter().map(|(_, m)| m.as_str()).collect();
        assert_eq!(
            &texts[..9],
            &[
                "Ann is 42 years, 1.5%",
                "{\"a\":1} and [1]",
                " styled",
                "NaN NaN",
                "a b {\"c\":2}",
                "100%",
                "1 %s",
                "<invalid value>",
                "Assertion failed: value=3",
            ]
        );
        assert!(
            texts[9].starts_with("%s: ") && texts[9].ends_with("ms kept"),
            "timeLog prints its data without formatting it: {:?}",
            texts[9]
        );
    }

    #[test]
    fn members_need_no_receiver_and_a_trace_survives_its_stack() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        let found = lines(
            9_507,
            r#"var g = console.groupCollapsed, l = console.log, e = console.groupEnd;
               g("held apart"); l("inside"); e(); l("outside");
               Error.prepareStackTrace = function () { throw "boom"; };
               console.trace("still a line");
               delete Error.prepareStackTrace;"#,
        );
        assert_eq!(
            found[..3].to_vec(),
            vec![
                (1, "held apart".to_string()),
                (1, "  inside".to_string()),
                (1, "outside".to_string()),
            ]
        );
        assert_eq!(
            found[3],
            (1, "Trace: still a line".to_string()),
            "{found:?}"
        );
    }

    #[test]
    fn clear_empties_the_group_stack() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        let found = lines(
            9_508,
            r#"console.group("g"); console.log("in"); console.clear(); console.log("out");"#,
        );
        assert_eq!(
            found,
            vec![
                (1, "g".to_string()),
                (1, "  in".to_string()),
                (1, "out".to_string())
            ]
        );
    }

    #[test]
    fn the_rest_write_what_they_were_given() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        let found = lines(
            9_505,
            r#"console.dir({ a: 1 }); console.dirxml("x", 2); console.table([1, 2]); console.clear();
               console.groupCollapsed("g"); console.log("inside"); console.groupEnd(); console.log("outside");
               console.trace("here");"#,
        );
        assert_eq!(
            found[..6].to_vec(),
            vec![
                (1, "{\"a\":1}".to_string()),
                (1, "x 2".to_string()),
                (1, "[1,2]".to_string()),
                (1, "g".to_string()),
                (1, "  inside".to_string()),
                (1, "outside".to_string()),
            ]
        );
        assert!(
            found[6].0 == 1
                && found[6].1.starts_with("Trace: here\n")
                && found[6].1.contains("<test:console>"),
            "a trace names where it was called: {found:?}"
        );
    }
}
