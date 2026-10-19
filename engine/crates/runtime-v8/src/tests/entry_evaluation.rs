//! An entry module that throws fails `evaluate_module`; one that finishes does not, however
//! many modules it imports.
//!
//! deno_core settles a module evaluation that rejected by resolving the evaluation with `Ok`
//! and rethrowing the exception, for the event loop to surface as an unhandled rejection. The
//! engine's rejection handler hands every unhandled rejection to the game's listeners and
//! reports it handled -- which is right for a game's own promise and swallowed this one. An
//! entry that threw on its first line was logged "module evaluated", the host was told the
//! game was ready, and the player sat on a blank surface with a console line for a trace.
//!
//! And an entry that imports: deno_core reads a module's static imports concurrently, and each
//! read was charged the 100 MiB a read may be at most against the scheduler's pending-byte
//! budget, which refused the second -- so an entry with two imports did not load at all.

#[cfg(test)]
mod entry_evaluation_tests {
    use std::path::PathBuf;

    use shared::vfs::GamePaths;

    use super::super::support::test_host_state;
    use crate::HostJsRuntime;

    const GAME: &str = "entry-evaluation";

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn evaluate(entry: &str) -> Result<(), String> {
        evaluate_package(&[("game.js", entry)]).await
    }

    async fn evaluate_package(files_in_code: &[(&str, &str)]) -> Result<(), String> {
        let root = Scratch(std::env::temp_dir().join(format!(
            "migo-entry-evaluation-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
        let (files, cache) = (root.0.join("files"), root.0.join("cache"));
        let paths = GamePaths::new(&files, &cache, GAME, 1).expect("game paths");
        std::fs::create_dir_all(paths.code_dir()).expect("code dir");
        for (name, source) in files_in_code {
            std::fs::write(paths.code_dir().join(name), source).expect("module");
        }

        let mut state = test_host_state();
        state.app_files_dir = files;
        state.app_cache_dir = cache.clone();
        let mut runtime = HostJsRuntime::new(
            1,
            state,
            &cache,
            #[cfg(feature = "v8-limits")]
            crate::V8LimitsConfig::default(),
            #[cfg(feature = "code-signing")]
            false,
            #[cfg(feature = "code-signing")]
            None,
        );
        runtime
            .evaluate_module(GAME.into(), "game.js".into())
            .await
            .map_err(|e| format!("{} {}", e.msg, e.detail.unwrap_or_default()))
    }

    #[tokio::test]
    async fn an_entry_that_throws_fails() {
        let failure = evaluate("globalThis.before = 1;\nnotDefinedAnywhere();\n")
            .await
            .expect_err("an entry that threw on its second line must not evaluate");
        assert!(
            failure.contains("notDefinedAnywhere is not defined"),
            "the failure names the exception: {failure}"
        );
    }

    #[tokio::test]
    async fn an_entry_that_throws_after_an_await_fails() {
        let failure =
            evaluate("await Promise.resolve();\nthrow new RangeError('after the await');\n")
                .await
                .expect_err("an entry that threw once resumed must not evaluate");
        assert!(
            failure.contains("after the await"),
            "the failure names the exception: {failure}"
        );
    }

    #[tokio::test]
    async fn an_entry_that_finishes_evaluates() {
        evaluate("globalThis.done = true;\nawait Promise.resolve();\n")
            .await
            .expect("an entry that finished evaluates");
    }

    // Each import is larger than the scheduler reads inline, so all four go through its
    // pending-byte admission, where the reads overlapped. Whether they do overlap is the
    // timing's; `loader::tests` holds the charge itself, which is what made them collide.
    #[tokio::test]
    async fn an_entry_that_imports_several_modules_evaluates() {
        let padding = " ".repeat(20 * 1024);
        let modules = ["a", "b", "c", "d"].map(|name| {
            (
                format!("{name}.js"),
                format!("globalThis.loaded = (globalThis.loaded || 0) + 1; export const {name} = 1;\n//{padding}\n"),
            )
        });
        let mut package: Vec<(&str, &str)> = modules
            .iter()
            .map(|(n, s)| (n.as_str(), s.as_str()))
            .collect();
        package.push((
            "game.js",
            "import './a.js'; import './b.js'; import './c.js'; import './d.js';\n\
             if (globalThis.loaded !== 4) throw new Error('loaded ' + globalThis.loaded);\n",
        ));
        evaluate_package(&package)
            .await
            .expect("an entry and its four imports load and evaluate");
    }

    // A promise the game rejected and left alone is the game's, reported to its listeners: it
    // does not make the entry a failure.
    #[tokio::test]
    async fn a_rejection_the_entry_leaves_alone_is_not_its_failure() {
        evaluate("Promise.reject(new Error('the game left this one'));\nglobalThis.done = true;\n")
            .await
            .expect("an entry that finished evaluates, whatever it left rejected");
    }
}
