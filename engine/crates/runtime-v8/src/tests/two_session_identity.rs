//! Two live Sessions, two game identities, and what separates their storage.
//!
//! `storage_isolation.rs` proves the resolver gives two `GamePaths` two
//! different files. It hands the resolver a `GamePaths` it built itself, which
//! leaves the more interesting half unproven: that a real `evaluate_module`
//! turns a *game id* into that `GamePaths`, and that two Sessions alive at once
//! hold two different ones rather than sharing a slot. The concurrent-identity
//! isolation is a claim about two Sessions, and every test of it so far has been
//! a claim about one function given distinct inputs.
//!
//! **The recorded obstacle was that a Session with no surface never reaches
//! `evaluate_module`.** It does. `HostJsRuntime::new` takes no surface —
//! `spawn_host_thread`/`Host::new` is what needs one, a layer above where the
//! identity binds. The layer that can *see* this property is below the layer the
//! plan named.
//!
//! **A Session per thread, because V8 makes that the only option.** Two
//! `HostJsRuntime`s on one thread abort the process with
//! `Fatal error in v8::HandleScope::CreateHandle(): Cannot create a handle
//! without a HandleScope` — the current isolate is thread-local and each runtime
//! expects to own it. That is not a limitation of the fixture: a real concurrent
//! Session *is* a host thread, so one runtime per thread is the shape under test.
//! It does mean every observation has to cross a channel, and the two loads have
//! to be *ordered* rather than raced: a shared identity slot written by two
//! concurrent threads is a coin flip, and a kill has to be certain.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::time::Duration;

    use shared::{
        channel::ThreadWakeup,
        device::gpu_caps::GpuCaps,
        op_state::{AudioSender, HostOpState, NetworkPolicy},
        render_command_sender::CommandSender,
        vfs::game_paths::GamePaths,
    };

    use crate::host_runtime::HostJsRuntime;

    /// Long enough that a loaded V8 isolate on a busy machine is not mistaken for
    /// a hang, short enough that a genuine hang fails the suite instead of
    /// stalling it. Every cross-thread wait in this file is bounded.
    const ANSWER_TIMEOUT: Duration = Duration::from_secs(60);

    /// One scratch tree per test, named so a leftover directory says which test
    /// left it. Unique per process and per call: two runs of the suite must not
    /// resolve into the same `games/` root and agree by accident.
    fn scratch(tag: &str) -> PathBuf {
        let unique = format!(
            "migo-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after the epoch")
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&root).expect("create scratch root");
        root
    }

    /// The same `GamePaths` the runtime will derive, used both to place the entry
    /// module and to name the namespace each game must land in. Derived rather
    /// than spelled out: the on-disk layout is `game_paths.rs`'s property and has
    /// its own tests, and repeating it here would make this test fail on a layout
    /// change while saying nothing about identity binding.
    ///
    /// The session id is arbitrary because only `code_dir` and `user_data_dir` are
    /// read from the result, and neither is session-scoped — `/tmp` is the only
    /// one that is.
    fn paths(files: &Path, cache: &Path, game_id: &str) -> GamePaths {
        GamePaths::new(files, cache, game_id, 1).expect("game paths")
    }

    /// A trivial entry module where `evaluate_module` will look for it. It must
    /// exist and must evaluate: a load or evaluation failure returns `Err` before
    /// the identity is observable.
    fn install_entry(files: &Path, cache: &Path, game_id: &str) {
        let code = paths(files, cache, game_id).code_dir().to_path_buf();
        std::fs::create_dir_all(&code).expect("create code dir");
        std::fs::write(
            code.join("game.js"),
            b"// nothing to draw; the identity binds before any of this runs\n",
        )
        .expect("write entry module");
    }

    fn host_op_state(files: &Path, cache: &Path, id: i32) -> HostOpState {
        let (render_tx, _render_rx) = CommandSender::new();
        let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);

        HostOpState {
            callback_ids: std::sync::Arc::new(shared::callback_id::CallbackIdAllocator::default()),
            runtime_generation: 1,
            id,
            app_cache_dir: cache.to_path_buf(),
            app_files_dir: files.to_path_buf(),
            code_dir: None,
            game_paths: None,
            vfs: None,
            mount_table: None,
            render_tx,
            text_measurer: None,
            audio_tx: AudioSender::new(shared::audio_channel::disconnected(), ThreadWakeup::new()),
            host_tx,
            device_services: None,
            raf_rx: None,
            raf_demand: Arc::new(shared::raf_signal::RafDemand::new()),
            request_vsync: None,
            sub_packages: Vec::new(),
            workers_path: None,
            network_policy: NetworkPolicy::default(),
            backgrounded: Arc::new(AtomicBool::new(false)),
            timer_backgrounded: Arc::new(AtomicBool::new(false)),
            webgl_context_created: Arc::new(AtomicBool::new(false)),
            context_lost: Arc::new(shared::op_state::ContextLostState::default()),
            code_signing_enabled: false,
            gpu_caps: GpuCaps::new(),
        }
    }

    /// The text texture cache *this Session's own* canvas op state is bound to.
    ///
    /// Read out of the live op state rather than resolved from a host id the test
    /// picks, because the claim under test is that a Session binds its own — a test
    /// that called `text_cache_for_host(1)` itself would be asserting that a registry
    /// separates two keys, which is what the registry's own tests already do.
    fn text_cache_of(rt: &HostJsRuntime) -> shared::text_texture_cache::SharedTextCache {
        let op_state = rt.op_state();
        let borrowed = op_state.borrow();
        borrowed
            .borrow::<shared::op_state::CanvasOpState>()
            .text_cache
            .clone()
    }

    /// A cache key that differs only in its text, so the label is the only thing that
    /// can separate two entries and a collision cannot come from anything else.
    fn text_key(label: &str) -> shared::text_texture_cache::TextCacheKey {
        shared::text_texture_cache::TextCacheKey {
            text: label.to_string(),
            font_request: "16px sans-serif".to_string(),
            font_size_bits: 16.0f32.to_bits(),
            font_weight: 400,
            italic: false,
            fill_color: 0xFFFF_FFFF,
            text_align: shared::protocol::render_cmd::TextAlign::Left,
            text_baseline: shared::protocol::render_cmd::TextBaseline::Alphabetic,
            canvas_w: 64,
            canvas_h: 32,
            font_generation: 0,
        }
    }

    fn storage_of(rt: &HostJsRuntime) -> PathBuf {
        let op_state = rt.op_state();
        let borrowed = op_state.borrow();
        crate::storage::storage_dir(&borrowed).expect("a loaded game must resolve storage")
    }

    enum Step {
        /// Bind this game identity by evaluating its entry module.
        Load(String),
        /// Ask the production resolver, on this thread, over this isolate's own
        /// op state.
        ReportStorage,
        /// Write one key through the production storage path, under the shipped
        /// quota, and report whether it was admitted.
        Write { key: String, bytes: usize },
        /// This game's own byte total, as its own store accounts for it.
        ReportBytes,
        /// Put a text texture under `label` in whatever cache *this* Session's own
        /// canvas op state is bound to.
        CacheText { label: String, texture_id: u32 },
        /// Whether this Session's own cache holds `label`, and under which texture
        /// name. `None` means its own cache has never seen it.
        LookUpText { label: String },
    }

    /// What a step answered. One channel carries all three because the steps are
    /// ordered: a Session answers exactly one of these per step it is handed.
    enum Answer {
        Storage(PathBuf),
        /// `Ok(())` when the write was admitted, `Err(message)` when the quota
        /// refused it. The message is carried rather than the error so a refusal
        /// for the wrong reason is visible in the failure.
        Written(Result<(), String>),
        Bytes(u64),
        /// The texture name this Session's own cache holds for a label, if any.
        Texture(Option<u32>),
    }

    /// A host thread with one live `HostJsRuntime`, driven step by step so the
    /// two Sessions' loads can be ordered instead of raced.
    struct Session {
        steps: Option<mpsc::Sender<Step>>,
        answers: mpsc::Receiver<Answer>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Session {
        fn spawn(files: &Path, cache: &Path, id: i32) -> Self {
            let (step_tx, step_rx) = mpsc::channel::<Step>();
            let (answer_tx, answer_rx) = mpsc::channel::<Answer>();
            let (files, cache) = (files.to_path_buf(), cache.to_path_buf());

            let thread = std::thread::Builder::new()
                .name(format!("session-{id}"))
                .spawn(move || {
                    let tokio_rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("current-thread tokio runtime");
                    tokio_rt.block_on(async move {
                        let mut js = HostJsRuntime::new(
                            id,
                            host_op_state(&files, &cache, id),
                            &cache,
                            #[cfg(feature = "v8-limits")]
                            crate::host_runtime::V8LimitsConfig::from_max_memory_mb(256),
                            #[cfg(feature = "code-signing")]
                            false,
                            #[cfg(feature = "code-signing")]
                            None,
                        );
                        // Ends when the last `steps` sender drops, which is the
                        // shutdown signal.
                        while let Ok(step) = step_rx.recv() {
                            let answer = match step {
                                Step::Load(game_id) => {
                                    js.evaluate_module(game_id.clone(), "game.js".to_string())
                                        .await
                                        .unwrap_or_else(|e| panic!("{game_id} must evaluate: {e}"));
                                    Answer::Storage(PathBuf::new())
                                }
                                Step::ReportStorage => Answer::Storage(storage_of(&js)),
                                Step::Write { key, bytes } => {
                                    let dir = storage_of(&js);
                                    let value = "x".repeat(bytes);
                                    Answer::Written(
                                        migo_io::storage_ops::storage_set(
                                            &dir,
                                            &key,
                                            &value,
                                            crate::storage::MAX_TOTAL_BYTES,
                                        )
                                        .map_err(|e| e.to_string()),
                                    )
                                }
                                Step::CacheText { label, texture_id } => {
                                    // Deliberately asserts nothing. A setup assertion
                                    // here would make a cross-session hit die inside
                                    // this helper -- detecting the defect while never
                                    // evaluating the claim the test is named for. Under
                                    // a merged cache the second insert evicts the
                                    // first, and the eviction list is exactly the kind
                                    // of tempting setup assertion that hides where the
                                    // failure belongs.
                                    let cache = text_cache_of(&js);
                                    let _evicted = cache.lock().insert(
                                        text_key(&label),
                                        shared::text_texture_cache::CachedTextEntry {
                                            texture_id,
                                            width: 16,
                                            height: 16,
                                            size_bytes: 4 * 16 * 16,
                                        },
                                    );
                                    Answer::Texture(None)
                                }
                                Step::LookUpText { label } => Answer::Texture(
                                    text_cache_of(&js)
                                        .lock()
                                        .get(&text_key(&label))
                                        .map(|entry| entry.texture_id),
                                ),
                                Step::ReportBytes => {
                                    let dir = storage_of(&js);
                                    Answer::Bytes(
                                        migo_io::storage_ops::storage_info(
                                            &dir,
                                            crate::storage::MAX_TOTAL_BYTES,
                                        )
                                        .expect("a loaded game's store reports its own size")
                                        .current_bytes,
                                    )
                                }
                            };
                            if answer_tx.send(answer).is_err() {
                                break;
                            }
                        }
                    });
                })
                .expect("spawn session thread");

            Self {
                steps: Some(step_tx),
                answers: answer_rx,
                thread: Some(thread),
            }
        }

        fn step(&self, step: Step) -> Answer {
            self.steps
                .as_ref()
                .expect("session still running")
                .send(step)
                .expect("session thread accepts steps");
            self.answers
                .recv_timeout(ANSWER_TIMEOUT)
                .expect("session thread answered within the timeout")
        }

        fn load(&self, game_id: &str) {
            let _ = self.step(Step::Load(game_id.to_string()));
        }

        fn storage(&self) -> PathBuf {
            match self.step(Step::ReportStorage) {
                Answer::Storage(path) => path,
                _ => panic!("a storage step must answer with a path"),
            }
        }

        fn write(&self, key: &str, bytes: usize) -> Result<(), String> {
            match self.step(Step::Write {
                key: key.to_string(),
                bytes,
            }) {
                Answer::Written(outcome) => outcome,
                _ => panic!("a write step must answer with its outcome"),
            }
        }

        fn cache_text(&self, label: &str, texture_id: u32) {
            let _ = self.step(Step::CacheText {
                label: label.to_string(),
                texture_id,
            });
        }

        fn look_up_text(&self, label: &str) -> Option<u32> {
            match self.step(Step::LookUpText {
                label: label.to_string(),
            }) {
                Answer::Texture(found) => found,
                _ => panic!("a lookup step must answer with a texture name or nothing"),
            }
        }

        fn bytes(&self) -> u64 {
            match self.step(Step::ReportBytes) {
                Answer::Bytes(total) => total,
                _ => panic!("a size step must answer with a total"),
            }
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            // Dropping the sender is what ends the loop; the join is only
            // reached after every step was already acknowledged.
            self.steps = None;
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// The property concurrent isolation is actually about: two Sessions running at once,
    /// each writing to its own storage.
    ///
    /// **Both Sessions are given the *same* app directories on purpose.** Handing
    /// each its own `app_files_dir` would make the two paths differ for a reason
    /// that has nothing to do with per-game namespacing, and the test would pass
    /// over an engine that ignored the game id entirely. The game id is the only
    /// input that differs here, so it is the only thing that can separate them.
    ///
    /// **Both loads happen before either read, and that ordering is
    /// load-bearing.** Every way an identity can be shared — a process-wide slot
    /// written at bind time, a resolver that memoises its first answer, one op
    /// state behind two runtimes — shows up as two equal paths only once both
    /// Sessions have bound. Reading `a` before `b` loads would let all of them
    /// through.
    #[test]
    fn two_live_sessions_resolve_storage_under_their_own_game_id() {
        let root = scratch("two-sessions");
        let files = root.join("files");
        let cache = root.join("cache");
        install_entry(&files, &cache, "game-a");
        install_entry(&files, &cache, "game-b");

        let a = Session::spawn(&files, &cache, 1);
        let b = Session::spawn(&files, &cache, 2);
        a.load("game-a");
        b.load("game-b");

        let (sa, sb) = (a.storage(), b.storage());
        let na = paths(&files, &cache, "game-a")
            .user_data_dir()
            .to_path_buf();
        let nb = paths(&files, &cache, "game-b")
            .user_data_dir()
            .to_path_buf();

        assert_ne!(sa, sb, "both live Sessions resolved to {sa:?}");
        // Not merely different: each under the identity it was loaded with, and
        // under no other. Two paths can differ while both are wrong — a counter,
        // a host id, a temp name — and the negative half is what rules out "both
        // took game-b" and "both took a shared root".
        assert!(
            sa.starts_with(&na) && !sa.starts_with(&nb),
            "game-a resolved to {sa:?}, which is not its own namespace {na:?}"
        );
        assert!(
            sb.starts_with(&nb) && !sb.starts_with(&na),
            "game-b resolved to {sb:?}, which is not its own namespace {nb:?}"
        );

        drop(a);
        drop(b);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two live Sessions hold two text texture caches, proving isolation
    /// at the layer this test targets.
    ///
    /// The cache's own isolation tests take two host ids from
    /// `text_cache_for_host` and show the registry separates them. That is a claim
    /// about a registry given distinct keys — the same shape an earlier fix replaced for
    /// storage. What was never executed is the step before it: that a Session *binds*
    /// its own cache, through `CanvasOpState::for_host` in the `web` extension's state
    /// init, so two live Sessions end up on two caches without anyone choosing a key.
    /// This reads the cache out of each live op state instead of resolving one.
    ///
    /// **Both entries are cached before either lookup.** A cache keyed without session
    /// identity, or a process-wide slot written at bind time, or one op state behind
    /// two runtimes all show up as a cross-session hit only once both Sessions have
    /// written. Looking `a` up before `b` writes would let every one of them through.
    ///
    /// **Each Session's own hit is asserted too**, and it is not decoration: every
    /// negative assertion here is satisfied by a cache that stores nothing at all.
    #[test]
    fn two_live_sessions_hold_their_own_text_texture_cache() {
        let root = scratch("two-sessions-text-cache");
        let files = root.join("files");
        let cache = root.join("cache");
        install_entry(&files, &cache, "game-a");
        install_entry(&files, &cache, "game-b");

        // Host ids no other test in this process uses, because the cache registry is
        // process-wide and keyed by host id: a shared id would make two tests agree
        // by accident.
        let a = Session::spawn(&files, &cache, 8101);
        let b = Session::spawn(&files, &cache, 8102);
        a.load("game-a");
        b.load("game-b");

        // The *same* label from both Sessions, deliberately. Two different labels
        // would separate the entries by key and the test would pass over a cache with
        // no session identity at all. Different texture names, because equal ones pass
        // under a merge.
        a.cache_text("shared-label", 0xA11);
        b.cache_text("shared-label", 0xB22);

        assert_eq!(
            a.look_up_text("shared-label"),
            Some(0xA11),
            "game-a's own cache did not return game-a's texture: it either lost the \
             entry, or game-b's insert under the same label replaced it"
        );
        assert_eq!(
            b.look_up_text("shared-label"),
            Some(0xB22),
            "game-b was served game-a's texture name, which is meaningless in its own \
             GL context"
        );

        // And each must miss what only the other cached, which is the half a
        // same-label test can state and a different-label one cannot.
        assert_eq!(
            a.look_up_text("only-b-has-this"),
            None,
            "game-a found a label only game-b could have cached"
        );
        b.cache_text("only-b-has-this", 0xB33);
        assert_eq!(a.look_up_text("only-b-has-this"), None);
        assert_eq!(b.look_up_text("only-b-has-this"), Some(0xB33));

        drop(a);
        drop(b);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// One game exhausting its storage quota leaves the other's untouched.
    ///
    /// "Per-game filesystem, key-value, and quota isolation
    /// derived from the game identity" is an enforced property; an earlier fix closed
    /// the *namespace* half: two live Sessions resolve to two directories. Distinct
    /// directories are necessary and not sufficient. Nothing showed that the 10 MB
    /// limit is *each game's* 10 MB, which is a claim about where the accounting
    /// lives, not about where the files do — and the two are separable. The store
    /// handles are kept in a process-wide `HashMap` in `storage_ops`, and a shared
    /// running total, a cache key that lost the directory, or a quota checked against
    /// a global would all leave two distinct directories in place while making one
    /// game's writes count against the other's budget.
    ///
    /// **The exhaustion is the setup and the neighbour's write is the property.**
    /// `a` is filled through the production `storage_set` under the shipped
    /// `MAX_TOTAL_BYTES` until it is refused, and that refusal is asserted: a fixture
    /// that never reached the limit would prove nothing about sharing it. Then `b`
    /// writes, and must be admitted.
    ///
    /// **The byte totals are asserted as well as the outcome**, because "b's write
    /// succeeded" is satisfied by a shared store that simply had room left. `b`'s own
    /// store must account for `b`'s bytes and nothing near `a`'s.
    #[test]
    fn one_game_exhausting_its_quota_leaves_the_other_game_its_own() {
        let root = scratch("two-sessions-quota");
        let files = root.join("files");
        let cache = root.join("cache");
        install_entry(&files, &cache, "game-a");
        install_entry(&files, &cache, "game-b");

        let a = Session::spawn(&files, &cache, 1);
        let b = Session::spawn(&files, &cache, 2);
        a.load("game-a");
        b.load("game-b");

        // One below `MAX_VALUE_SIZE`, so the per-value cap never decides anything
        // here and the only limit in play is the total.
        const CHUNK: usize = 1024 * 1024 - 1;
        // The shipped total is 10 MB, so eleven chunks cannot fit however the store
        // rounds; the loop stops at the first refusal rather than at this bound.
        const ENOUGH_TO_OVERFILL: usize = 16;

        let mut refusal = None;
        for chunk in 0..ENOUGH_TO_OVERFILL {
            if let Err(message) = a.write(&format!("fill-{chunk}"), CHUNK) {
                refusal = Some((chunk, message));
                break;
            }
        }
        let (chunks_admitted, message) = refusal
            .expect("game-a never reached its quota, so nothing here says whose quota it was");
        assert!(
            message.contains("storage limit exceeded"),
            "game-a's write was refused for something other than its quota: {message}"
        );
        assert!(
            chunks_admitted > 1,
            "game-a was refused its {chunks_admitted}th chunk, which is too early for a \
             10 MB limit -- the refusal is not the quota it is named for"
        );

        assert_eq!(
            b.write("after-the-neighbour-filled-up", CHUNK),
            Ok(()),
            "game-b was refused because game-a had filled up"
        );

        let (bytes_a, bytes_b) = (a.bytes(), b.bytes());
        assert!(
            bytes_b < bytes_a,
            "game-b's store accounts for {bytes_b} bytes against game-a's {bytes_a}, so \
             the two are counting the same writes"
        );
        assert!(
            bytes_b >= CHUNK as u64 && bytes_b < 2 * CHUNK as u64,
            "game-b's store accounts for {bytes_b} bytes after writing one {CHUNK}-byte \
             value, so it is not accounting for its own writes alone"
        );

        drop(a);
        drop(b);
        let _ = std::fs::remove_dir_all(&root);
    }
}
