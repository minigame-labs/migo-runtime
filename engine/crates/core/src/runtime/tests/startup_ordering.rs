//! Parallel GPU/V8 startup ordering, pinned against the source.

const HOST: &str = include_str!("../host.rs");
const SHELL: &str = include_str!("../shell.rs");
const SESSION_THREAD: &str = include_str!("../session_thread.rs");
const THREAD: &str = include_str!("../thread.rs");
const RENDER_SERVICE: &str = include_str!("../../services/render.rs");
const RENDER_THREAD: &str = include_str!("../../../../graphics/src/render_thread.rs");
const CANVAS_MANAGER: &str = include_str!("../../../../graphics/src/canvas/manager/mod.rs");
const GPU_CAPS: &str = include_str!("../../../../shared/src/device/gpu_caps.rs");
const CONTROL: &str = include_str!("../../../../shared/src/surface/control.rs");
const RENDER_CMD: &str = include_str!("../../../../shared/src/protocol/render_cmd.rs");
const HOST_CMD: &str = include_str!("../../../../shared/src/protocol/host_cmd.rs");
const EXTERNAL: &str = include_str!("../external.rs");
const REGISTRY: &str = include_str!("../registry.rs");
const CAPI_SURFACE: &str = include_str!("../../../../capi/src/surface.rs");
/// Android's `HostNotifier`. Once the one platform whose host could not hear a surface
/// loss -- `notify_surface_lost` was implemented only by the C boundary's `CapiHostKit`
/// and this impl took the trait's no-op default, which is what forced a refused Surface
/// to latch `gpu_caps` instead.
const ANDROID_PLATFORM: &str = include_str!("../../../../platform/src/android/platform.rs");

/// A region with its line comments removed.
///
/// Every "must not contain" assertion below goes through this, and it is not
/// fastidiousness: the comments in this codebase deliberately name the thing they
/// explain, so a comment reading "deliberately not gpu_caps.set_failed" satisfies a
/// search for `set_failed`, and the assertion that the call is absent then fails on
/// the prose saying it is absent. That happened. Rewording the comment would have
/// worked until the next person wrote a clear one.
///
/// Line comments only. A block comment inside one of these regions would need
/// handling and there are none; a `//` inside a string literal would be stripped
/// wrongly and there are none of those either. Either appearing is cheap to notice,
/// because the assertion goes green and its injection proof goes green with it.
fn code_only(region: &str) -> String {
    region
        .lines()
        .map(|line| match line.find("//") {
            Some(offset) => &line[..offset],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn host_new_body() -> &'static str {
    let start = HOST
        .find("pub(crate) fn new(")
        .expect("Host::new must remain present");
    let end = HOST[start..]
        .find("pub(crate) async fn handle_command")
        .map(|offset| start + offset)
        .expect("Host::new must end before handle_command");
    &HOST[start..end]
}

#[test]
fn render_starts_before_v8_and_host_never_waits_for_the_gpu() {
    let body = host_new_body();
    // The render thread is launched by the session shell, which is engine-neutral
    // and shared with the external-frame execution. That makes "render before V8"
    // structural rather than an ordering someone has to preserve: the shell
    // returns before any line of `Host::new` can name a JavaScript runtime, and
    // the shell has no way to name one.
    assert!(
        SHELL.contains("RenderService::new("),
        "the session shell must be what starts RenderService"
    );
    assert!(
        !SHELL.contains("HostJsRuntime"),
        "the session shell must stay free of the embedded engine; it is compiled \
         into the external-frame product, which links none"
    );
    let render = body
        .find("SessionShell::build(")
        .expect("Host::new must build the session shell, which starts RenderService");
    let js = body
        .find("HostJsRuntime::new(")
        .expect("Host::new must construct V8 on the host thread");
    let watchdog = body
        .find("install_watchdog")
        .expect("watchdog installation must remain present");
    let assemble = body
        .find("let host = Self {")
        .expect("Host must be assembled before publication");

    assert!(
        render < js && js < watchdog && watchdog < assemble,
        "required startup order is render -> V8 -> watchdog -> Host"
    );

    // `Host::new` runs on the host thread while the caller that asked for a
    // session is blocked, so anything it waits for is time a host waits before
    // it can start a game. It waited for GPU readiness here once, at a measured
    // 30-44 ms; nothing between here and the first line of launch JS reads the
    // capabilities, so the wait belongs at that point instead. See
    // `gpu_readiness_is_joined_before_any_launch_js`.
    assert!(
        !body.contains("wait_ready("),
        "Host::new must not block on GPU readiness; the wait belongs in ensure_gpu_ready"
    );
}

#[test]
fn gpu_readiness_is_joined_before_any_launch_js() {
    // The property: no JS supplied for a launch -- prelude or module -- may run
    // against the provisional all-false capability snapshot, because image ops
    // read it to choose upload paths. `on_eval_script` is deliberately not
    // covered: it evaluates host-supplied source on a host-driven command, and
    // is not part of launching a game.
    let wait = HOST
        .find("fn ensure_gpu_ready(")
        .expect("the deferred GPU join must remain present");
    let wait_body = &HOST[wait..];
    let deadline = wait_body
        .find("wait_ready_until(self.gpu_init_started, GPU_INIT_TIMEOUT)")
        .expect("the join must use the budget that started when the render thread launched");
    assert!(
        deadline
            < wait_body
                .find("fn ")
                .map(|_| wait_body.len())
                .unwrap_or(wait_body.len()),
        "deadline lookup must stay inside ensure_gpu_ready"
    );

    let start = HOST
        .find("async fn on_evaluate_module(")
        .expect("the launch path must remain present");
    let end = HOST[start..]
        .find("\n    fn ")
        .map(|offset| start + offset)
        .expect("on_evaluate_module must end");
    let body = &HOST[start..end];

    let ready = body
        .find("self.ensure_gpu_ready()?")
        .expect("the launch path must join GPU readiness");
    let prelude = body
        .find("exec_script_owned(")
        .expect("prelude execution must remain present");
    let module = body
        .find(".evaluate_module(")
        .expect("module evaluation must remain present");
    assert!(
        ready < prelude && ready < module,
        "GPU capabilities must be published before any prelude or module JS runs"
    );
}

#[test]
fn a_launch_that_fails_is_announced_like_one_that_succeeds() {
    // The property: the two outcomes of loading content travel the same way, from
    // the same place, for every caller.
    //
    // Only one of them did. `launch_content` ends in `notify_game_ready`, and its
    // `Err` reached `handle_command`, which logs every failure and reports none --
    // so a game that did not start told the embedder nothing, on the one platform
    // that ships. That silence covers the renderer failing to initialise, because
    // `ensure_gpu_ready` raises it from this path.
    //
    // Pinned on the *pairing function*, not on a caller. A report attached to the
    // launch command alone left the other caller -- a restart, which reloads the
    // previous module through the same path -- announcing its successes and
    // swallowing its failures. Review caught that; this assertion is what would.
    let pairing = HOST
        .split("async fn on_evaluate_module(")
        .nth(1)
        .expect("the launch pairing must remain present")
        .split("\n    async fn ")
        .next()
        .expect("the pairing must end before the function it wraps");
    assert!(
        pairing.contains("self.platform.notify_error("),
        "a launch that fails must reach the host through on_error"
    );
    assert!(
        pairing.contains("self.launch_content(game_id, entry).await"),
        "the pairing must wrap the work rather than duplicate it, so no caller can \
         reach the work without the announcement"
    );
    assert!(
        !code_only(pairing).contains("should_notify_error"),
        "the launch report must not be throttled: it runs once per launch, so a \
         suppressed first report would be the only report there was"
    );

    let work = HOST
        .split("async fn launch_content(")
        .nth(1)
        .expect("the launch path must remain present");
    assert!(
        work.contains("self.platform.notify_game_ready(self.id);"),
        "the success half of the pairing must remain present, or this test is \
         asserting a symmetry that no longer has two sides"
    );

    // And every caller goes through the pairing, which is what makes it one.
    let callers = HOST.matches("self.launch_content(").count();
    assert_eq!(
        callers, 1,
        "only the pairing may call the work; a second caller would be a second \
         chance to swallow a failure"
    );
}

#[test]
fn installing_a_surface_retries_for_both_products_from_one_place() {
    // The property: a transient failure to install is retried, whichever execution
    // asked.
    //
    // Only the embedded one retried. Its reason applies to both -- a transiently full
    // render command queue makes the bounded-blocking recreate time out, and a
    // dropped recreate strands the app on a black frame with no further surface
    // callback coming -- but the retry sat in that execution's command handler, so
    // the external-frame product reported the timeout and gave up. Same pressure,
    // one product stranded.
    //
    // Pinned as "one place", not "both places": a second copy is how the two came to
    // disagree, and would let them disagree again.
    assert!(
        RENDER_SERVICE.contains("const INSTALL_ATTEMPTS: u32 = 3;"),
        "the attempt bound must be named where the retry runs"
    );
    let update = RENDER_SERVICE
        .split("pub(crate) fn update_surface(")
        .nth(1)
        .expect("update_surface must remain present")
        .split("\n    fn ")
        .next()
        .expect("update_surface must end before the next method");
    assert!(
        update.contains("attempts < Self::INSTALL_ATTEMPTS"),
        "the retry must live in the service, so both executions get it"
    );
    assert!(
        update.contains("lease.is_live()"),
        "a retired Surface must not be retried for: the host has taken it back and a \
         later attempt would arbitrate over something that no longer exists"
    );

    for (source, which) in [(HOST, "the embedded"), (EXTERNAL, "the external-frame")] {
        assert!(
            !code_only(source).contains("attempts < 3"),
            "{which} execution must not carry its own copy of the retry"
        );
    }
}

#[test]
fn an_install_whose_reply_was_given_up_on_is_still_reconciled() {
    // The property: the outcome of an install cannot be lost.
    //
    // `RenderService` gives up on the recreate reply after 500 ms while the request
    // stays queued, so the renderer can install afterwards. The session's slot then
    // stayed uncommitted while the renderer held a live Surface, and -- because the
    // resume runs only on the reply's success -- the renderer stayed paused with
    // nothing coming. That is the same state a lost-and-replaced Surface used to end
    // in, reached by a different route, so closing one and not the other would have
    // left the bug reachable.
    //
    // Every successful install is reported, not only the ones whose reply was lost:
    // the worker cannot know whether anyone is still listening, and a report nobody
    // needs is a no-op.
    assert!(
        RENDER_THREAD.contains("report_surface_installed(revision);"),
        "the startup install must report what it installed"
    );
    assert!(
        RENDER_THREAD.contains("report_surface_installed(requested);"),
        "the update install must report what it installed"
    );
    assert!(
        HOST_CMD.contains("SurfaceInstalled {"),
        "the report must travel as a command, on the channel that must deliver it"
    );

    let confirm = RENDER_SERVICE
        .split("pub(crate) fn confirm_install(")
        .nth(1)
        .expect("the reconciliation must remain present")
        .split("\n    fn ")
        .next()
        .expect("it must end");
    // Compared before it is taken, and the ordering is the property. Retries publish as
    // they go, so a report for revision 1 can arrive while revision 3 is what this
    // service waits for -- taking first would discard 3 on the mismatch and leave 3's
    // own report with nothing to commit.
    let compare = confirm
        .find("if self.outstanding != Some(revision)")
        .expect("a report must be compared against the outstanding record");
    let clear = confirm
        .find("self.outstanding = None;")
        .expect("a matching report must clear the record");
    assert!(
        compare < clear,
        "the record must be compared before it is cleared, or a mismatched report \
         discards a publication that is still waiting"
    );

    // Only a request the queue accepted can ever be reported, so only that one is
    // recorded. One that never reached the queue would leave the service waiting for a
    // report nobody sends -- and, with the timeout suppressed while a reconciliation is
    // pending, leave the host told nothing either.
    assert!(
        RENDER_SERVICE.contains("self.outstanding = failure.in_flight;"),
        "only an in-flight request may be recorded as outstanding"
    );
    assert!(
        RENDER_SERVICE.contains("fn in_flight(") && RENDER_SERVICE.contains("fn not_enqueued("),
        "the two outcomes must be named apart at the point they are produced"
    );
    assert!(
        EXTERNAL.contains("!render.install_pending()"),
        "a timeout must not be announced while its install may still land: it would \
         report a failure for a slow recreate that then succeeds"
    );
    assert!(
        confirm.contains("self.surface_control.live_candidate_for(revision)"),
        "the lease must be read back from the level, which answers only while that \
         publication is live -- so a generation retired between the install and the \
         report arrives as None rather than as an attachment to publish"
    );

    // And the record must not be a pin. Keeping the lease here would hold the host's
    // native Surface until the report arrived, and past the point RELEASED is meant to
    // be publishable if it never did or if the host detached first -- which is the
    // defect this whole arrangement removes, reintroduced one layer up.
    let field = RENDER_SERVICE
        .split("    outstanding:")
        .nth(1)
        .expect("the outstanding record must remain present")
        .split(',')
        .next()
        .expect("its type must end");
    assert!(
        !field.contains("SurfaceLease"),
        "the outstanding record must name a publication, not hold a lease: the level \
         already holds it and hands it back only while it is live"
    );

    // And both executions reconcile, because both can give up on a reply.
    for (source, which) in [(HOST, "the embedded"), (EXTERNAL, "the external-frame")] {
        assert!(
            source.contains("confirm_install(revision)"),
            "{which} execution must reconcile the report"
        );
    }
}

#[test]
fn the_handover_onshow_defers_to_is_actually_performed() {
    // The property: whichever of the two arms declines to resume, the other does it.
    //
    // `OnShow` with no live Surface deliberately does not resume -- a renderer with
    // nothing to present into would run for nothing -- and its comment says the
    // resume belongs to the `UpdateSurface` that follows. That handover was never
    // implemented on this execution, and `SurfaceSystem` preserves `Paused` across
    // `on_surface_available`, so a Surface installed while paused presented nothing
    // until some unrelated `OnShow` arrived.
    //
    // It became reachable when the surface-loss callback started firing: a host that
    // hears its Surface is gone detaches, attaches a replacement, and stays
    // foregrounded throughout, so nothing else was coming.
    let handler = EXTERNAL
        .split("fn handle_command(")
        .nth(1)
        .expect("the external command handler must remain present");
    let show = handler
        .find("HostCommand::OnShow")
        .expect("OnShow must still be handled");
    let show_arm = &handler[show..handler[show..]
        .find("\n        HostCommand::")
        .map_or(handler.len(), |offset| show + offset)];
    assert!(
        show_arm.contains("render.has_live_surface()"),
        "OnShow must still decline to resume without a live Surface, which is what \
         makes the other arm's resume necessary"
    );

    let update = handler
        .find("HostCommand::UpdateSurface {")
        .expect("the surface update must still be handled");
    let update_arm = &handler[update
        ..handler[update..]
            .find("\n        HostCommand::")
            .map_or(handler.len(), |offset| update + offset)];
    assert!(
        update_arm.contains("render.resume();"),
        "the update must perform the resume OnShow deferred to it, or a Surface \
         installed while paused presents nothing"
    );
    assert!(
        update_arm.contains("audio.resume();"),
        "and resume audio with it, as OnShow would have"
    );
    assert!(
        update_arm.contains("!backgrounded.load(Ordering::Relaxed)"),
        "guarded on backgrounded, for the reason the embedded execution guards it: a \
         host may install a Surface while hidden, and resuming then runs render and \
         audio in the background"
    );
    // The guard is the whole point of the pairing, so the embedded half must still
    // have it too -- otherwise this asserts a symmetry with one side. It lives in one
    // method there now, shared by the update's own reply and the reconciliation of an
    // install this host had stopped waiting for: two copies is how two paths come to
    // disagree about a guard.
    let resume = HOST
        .split("fn resume_foreground_if_visible(&mut self) {")
        .nth(1)
        .expect("the embedded resume must remain a single named place")
        .split("\n    }")
        .next()
        .expect("it must end");
    assert!(
        resume.contains("if self.backgrounded.load(Ordering::Relaxed) {"),
        "the embedded execution must still gate its resume on being foregrounded"
    );
    assert_eq!(
        HOST.matches("self.resume_foreground_if_visible();").count(),
        2,
        "both places a Surface becomes usable must go through it: the update's reply \
         and the SurfaceInstalled reconciliation"
    );
}

#[test]
fn staleness_is_classified_as_cancellation_where_it_is_known() {
    // The property: "the host took its Surface back" carries one code, decided by
    // whichever site noticed, and an ordering error carries a different one.
    //
    // Both arbitration sites used to flatten every variant onto `InvalidOperation`.
    // The session decides whether to report on the code, so it could exclude
    // `Cancelled` -- as it does -- and still announce an ordinary attach/detach race
    // as MIGO_ERROR_INTERNAL, which is what the C boundary maps every engine error
    // onto. Excluding `InvalidOperation` instead would have swallowed the ordering
    // errors with it, so the classification has to happen where the variant is still
    // in hand.
    for (source, mapper, which) in [
        (
            RENDER_SERVICE,
            "fn transition_error(",
            "the session's arbitration",
        ),
        (RENDER_THREAD, "fn binding_error(", "the render binding's"),
    ] {
        let body = source
            .split(mapper)
            .nth(1)
            .unwrap_or_else(|| panic!("{which} error mapper must remain present"))
            .split("\n}")
            .next()
            .unwrap_or_else(|| panic!("{which} error mapper must end"));
        assert!(
            body.contains("StaleGeneration => ErrorCode::Cancelled"),
            "{which} rejection for a retired generation must be Cancelled, which is \
             what the session reads to mean \"do not report this\""
        );
        assert!(
            body.contains("ConflictingLiveGeneration"),
            "{which} mapper must still name the ordering error separately, or the \
             classification is a rename rather than a distinction"
        );
        assert!(
            body.contains("ErrorCode::InvalidOperation"),
            "{which} ordering error must keep a code the session does report"
        );
    }

    // And the consumer still keys on that one code, which is what makes deciding it
    // at the source worth anything.
    assert!(
        EXTERNAL.contains("error.code != ErrorCode::Cancelled"),
        "the session must decide on the code the mappers set"
    );
}

#[test]
fn the_external_session_tells_its_host_what_the_embedded_one_does() {
    // The property: a host-facing render failure reaches the host on both products.
    //
    // It reached it on one. The embedded execution has forwarded surface loss and
    // reported swap and context-recovery failures since those events existed; the
    // external execution logged all of them and reported none -- so on the Apple
    // product `MigoOnSurfaceLostFn`, which `session.h` declares and the C boundary
    // wires, could not fire at all. That was an omission rather than a decision, and
    // the decisions that *were* made are asserted alongside it so a later reading
    // cannot mistake one for the other.
    let handler = EXTERNAL
        .split("fn handle_command(")
        .nth(1)
        .expect("the external command handler must remain present");
    assert!(
        handler.contains("platform.notify_surface_lost(id, public_generation, reason);"),
        "surface loss must reach the callback session.h declares for it"
    );
    // Bound to the arm rather than to the handler: two arms here can report, so a
    // substring check on the whole handler would pass while this one stayed silent.
    let update = handler
        .find("HostCommand::UpdateSurface {")
        .expect("the surface update must still be handled");
    let update_arm = &handler[update
        ..handler[update..]
            .find("\n        HostCommand::")
            .map_or(handler.len(), |offset| update + offset)];
    assert!(
        update_arm.contains("platform.notify_error("),
        "a Surface the renderer cannot install must reach the host: the reply channel \
         that carried the reason ends in this arm, and attach has already returned"
    );
    // But not every failure of that update is a failure to report. `Cancelled` means
    // what the update was for is gone: either the host retired the Surface, in which
    // case the party being told is the party that did it, or the render worker is
    // gone, in which case `RenderExit` reports it with a reason this arm does not
    // have. It would otherwise reach the host as MIGO_ERROR_INTERNAL, because that is
    // what the C boundary maps every engine error onto -- an ordinary attach/detach
    // race announced as an internal engine fault.
    assert!(
        update_arm.contains("error.code != ErrorCode::Cancelled"),
        "a cancelled update must not be reported as a failure"
    );

    let drain = EXTERNAL
        .split("fn drain_render_events(")
        .nth(1)
        .expect("the external event drain must remain present");
    let recovered = drain
        .find("RenderEvent::ContextRecovered { success } => {")
        .expect("context recovery must still be handled");
    let swap = drain
        .find("RenderEvent::SwapFailed { message } => {")
        .expect("swap failure must still be handled");
    assert!(
        drain[recovered..swap].contains("if !success {"),
        "only an unrecoverable loss may be reported: on_error is delivered as \
         non-recoverable, and a loss on its own is followed by a recovery"
    );
    assert!(
        drain[swap..].contains("RENDER_ERROR_NOTIFY_MIN_INTERVAL"),
        "the swap report must be spaced: a Surface that rejects every swap produces \
         one event per frame and on_error is a call into host code"
    );

    // And the two deliberate silences, pinned so they stay deliberate.
    let lost = drain
        .find("RenderEvent::ContextLost => {")
        .expect("context loss must still be handled");
    assert!(
        !code_only(&drain[lost..recovered]).contains("notify_error"),
        "a recoverable context loss must not be reported; the unrecoverable case is \
         the ContextRecovered arm"
    );
    let content = drain
        .find("other => {")
        .expect("the remaining events must still be handled");
    assert!(
        !code_only(&drain[content..]).contains("notify_error"),
        "a failed drawing command is the producer's, and the producer is another \
         process that already learns about its own errors"
    );
}

#[test]
fn host_drop_destroys_v8_before_stopping_render() {
    // V8 must be destroyed on the thread that owns its isolate, and that thread
    // is also the one that stops GL -- so the isolate has to go first. This used
    // to be pinned only on `Host::new`'s GPU-failure path, which was one of the
    // ways a host is torn down; `Drop` is all of them.
    let body = HOST
        .split("impl Drop for Host {")
        .nth(1)
        .expect("Host must implement Drop");
    let drop_js = body
        .find("self.js.take_and_drop();")
        .expect("Host teardown must explicitly drop V8");
    let shutdown = body
        .find("self.render.shutdown();")
        .expect("Host teardown must stop render");
    assert!(
        drop_js < shutdown,
        "V8 must be destroyed before render shutdown on every teardown path"
    );
}

#[test]
fn startup_registrations_are_guarded_until_host_publication() {
    let body = host_new_body();
    // The guard moved to the shell with the registrations it protects: the console
    // buffer is registered during the neutral bring-up, so a guard living beside
    // `Host` would have been guarding something it could no longer see.
    assert!(SHELL.contains("struct HostStartupGuard"));
    assert!(SHELL.contains("impl Drop for HostStartupGuard"));
    assert!(SHELL.contains("startup_guard.mark_console_registered()"));
    // The frame clock's sender is deliberately not one of them any more, and
    // needing a branch here was the symptom rather than the fix. It lived in a
    // registry of its own, so retiring it was somebody's job on each of spawn
    // failure, startup failure and ordinary exit -- and on the external-frame
    // product nobody held that job, so every session leaked an entry. It now sits
    // in the Host registry handle, retired by the `unregister_sender` that every
    // exit path already goes through.
    assert!(
        !code_only(SHELL).contains("mark_vsync_registered"),
        "the frame clock's sender must not need guarding: its lifetime is the Host \
         registry handle's"
    );
    assert!(
        !code_only(SHELL).contains("register_vsync_sender"),
        "the shell must not register a frame clock behind the ingress's back"
    );
    assert!(
        REGISTRY.contains("vsync_tx: Option<crossbeam_channel::Sender<f64>>,"),
        "the Host registry handle must own the frame clock's sender"
    );
    // And it is still handed back armed, so the embedded half's own failure
    // paths are covered by it rather than by nothing.
    assert!(
        body.contains("mut startup_guard,"),
        "Host::new must take the armed guard from the shell"
    );
    let disarm = body
        .find("startup_guard.disarm();")
        .expect("successful Host construction must transfer cleanup ownership");
    let assemble = body
        .find("let host = Self {")
        .expect("Host must be assembled while the guard is armed");
    let publish = body.find("Ok(host)").expect("Host publication must remain");
    assert!(
        assemble < disarm && disarm < publish,
        "guard must remain armed through assembly and disarm immediately before publication"
    );
}

#[test]
fn a_session_is_handed_its_own_ingress_rather_than_looking_it_up() {
    // The property: `attach` never asks the registry for the session it is
    // starting.
    //
    // It used to. The session thread removes its own entry on the way out, so a
    // renderer that failed to initialise took the entry away while `attach` was
    // still walking towards it -- measured on the iOS simulator, `attach` lost that
    // race about two runs in three and answered MIGO_ERROR_INTERNAL where the other
    // third answered MIGO_OK, for one input. Every part of the ingress exists before
    // any thread does, so the registration hands it back and there is nothing to
    // lose.
    assert!(
        REGISTRY.contains(") -> HostIngress {"),
        "registration must answer with the ingress it published"
    );
    assert!(
        SESSION_THREAD.contains("pub(crate) ingress: HostIngress,"),
        "the spawn must carry the session's own ingress out"
    );
    assert!(
        !code_only(CAPI_SURFACE).contains("host_ingress("),
        "attach must not look up the session it is installing; that lookup is for \
         callers who only have an id, and whose question is whether it is still alive"
    );
    assert!(
        CAPI_SURFACE.contains("Some(started.ingress)"),
        "attach must install the ingress the spawn handed it"
    );
}

#[test]
fn render_service_drop_is_a_detached_shutdown_fallback() {
    assert!(RENDER_SERVICE.contains("impl Drop for RenderService"));
    let drop_impl = RENDER_SERVICE
        .split("impl Drop for RenderService")
        .nth(1)
        .expect("RenderService Drop implementation");
    assert!(drop_impl.contains("self.shutdown_detached();"));
}

#[test]
fn detached_shutdown_is_a_noop_after_joined_shutdown() {
    let detached = RENDER_THREAD
        .split("pub fn shutdown_detached(&mut self)")
        .nth(1)
        .expect("detached render shutdown must remain present");
    let handle = detached
        .find("let Some(h) = self.handle.take() else")
        .expect("detached shutdown must return when the join handle is gone");
    let send = detached
        .find("self.cmd_tx.send(RenderCommand::Shutdown)")
        .expect("live render shutdown must send the shutdown command");
    assert!(
        handle < send,
        "the handle must be claimed before sending so repeated shutdown is a no-op"
    );
}

#[test]
fn shutdown_request_cannot_be_lost_when_the_command_queue_is_full() {
    assert!(
        RENDER_THREAD.contains("shutdown_requested: Arc<std::sync::atomic::AtomicBool>"),
        "RenderThread must retain an out-of-band shutdown level"
    );

    for function in [
        "pub fn shutdown(&mut self)",
        "pub fn shutdown_detached(&mut self)",
    ] {
        let body = RENDER_THREAD
            .split(function)
            .nth(1)
            .unwrap_or_else(|| panic!("{function} must remain present"));
        let handle = body
            .find("self.handle.take()")
            .expect("shutdown must claim the join handle");
        let request = body
            .find("self.shutdown_requested.store(true, Ordering::Release)")
            .expect("shutdown must publish the out-of-band exit level");
        let send = body
            .find("self.cmd_tx.send(RenderCommand::Shutdown)")
            .expect("shutdown command must still wake an idle receiver");
        assert!(
            handle < request && request < send,
            "claim handle, publish exit level, then send the best-effort wake"
        );
    }

    let recovery = RENDER_THREAD
        .find("// --- Deferred EGL context recovery ---")
        .expect("render loop recovery marker must remain present");
    let loop_start = RENDER_THREAD[..recovery]
        .rfind("loop {")
        .expect("main render loop must remain present");
    let loop_prefix = &RENDER_THREAD[loop_start..recovery];
    assert!(loop_prefix.contains("shutdown_requested.load(Ordering::Acquire)"));
}

#[test]
fn gpu_failure_detail_is_initialized_before_ready_publication() {
    let body = GPU_CAPS
        .split("pub fn set_failed")
        .nth(1)
        .expect("GpuCaps::set_failed must remain present");
    let detail = body
        .find("*failure = Some(detail)")
        .expect("failure detail must be stored");
    let failed = body
        .find("self.failed.store(true, Ordering::Release)")
        .expect("failure level must be published");
    let ready = body
        .find("self.ready.store(true, Ordering::Release)")
        .expect("ready level must be published");
    assert!(
        detail < failed && failed < ready,
        "failure detail must happen-before the ready publication"
    );
}

#[test]
fn a_refused_surface_does_not_declare_the_gpu_unusable() {
    // What a refused *onscreen* install may and may not touch.
    //
    // `gpu_caps` answers "are the published capability values real", and after this
    // failure they are: they come from `DeviceCapabilities::detect` against the
    // resource context, which is up, and `publish_gpu_caps` reads nothing an onscreen
    // surface contributes to. `set_failed` also latches, so declaring failure here made
    // `ensure_gpu_ready` fail for the rest of the session -- content could never start
    // -- while the surface loss reported alongside told the host to attach a Surface
    // this session could then never use. A recoverable failure made permanent.
    //
    // It survived for one reason, and this test used to assert that reason so it would
    // expire by itself: publishing Ready makes the state indistinguishable from a warm
    // start, which legitimately begins with no Surface, so the launch cannot treat it as
    // a failure -- leaving the loss as the only signal, and on Android the loss reached
    // nobody. Android delivering it is what retired the compromise, so what was the
    // blocker assertion is now the delivery assertion: the latch may not come back
    // while the signal that replaced it exists, and the signal may not quietly go away.
    assert!(
        ANDROID_PLATFORM.contains("fn notify_surface_lost"),
        "AndroidPlatform must keep delivering a Surface loss: it is what a host learns \
         from instead of a latched gpu_caps, and removing it would silently restore \
         'content announces ready, draws nothing, reports nothing'"
    );
    assert!(
        ANDROID_PLATFORM.contains("jni::notify_surface_lost("),
        "and it must reach Java, not stop at the trait impl"
    );

    let manager_start = CANVAS_MANAGER
        .find("pub(crate) fn new_with_resource(")
        .expect("CanvasManager constructor must remain present");
    let manager_end = CANVAS_MANAGER[manager_start..]
        .find("fn new_canvas_id")
        .map(|offset| manager_start + offset)
        .expect("CanvasManager constructor must end before new_canvas_id");
    assert!(
        !CANVAS_MANAGER[manager_start..manager_end].contains("gpu_caps.set("),
        "CanvasManager construction must not publish caps before it has detected them"
    );
    assert!(CANVAS_MANAGER.contains("pub(crate) fn publish_gpu_caps(&self)"));

    // Derived rather than listed: construction failing and a panic before it finished.
    // Both are the GPU genuinely not coming up. A third appearing is the drift to
    // catch, and the third that used to be here was the refused Surface.
    let failures = RENDER_THREAD.matches("gpu_caps.set_failed(").count();
    assert_eq!(
        failures, 2,
        "only the GPU failing to come up may declare it unusable -- construction and \
         the panic barrier -- and {failures} sites do"
    );

    // The publication itself is no longer gated on a flag: reaching it means
    // construction succeeded, which is the whole of what capabilities depend on.
    assert!(
        RENDER_THREAD.contains("cm.publish_gpu_caps();"),
        "the capabilities must still be published"
    );
    assert!(
        !code_only(RENDER_THREAD).contains("startup_failed"),
        "the flag that gated the publication is gone, and nothing replaced it"
    );
}

#[test]
fn the_initial_surface_is_claimed_after_gpu_init_and_by_one_route_only() {
    // The property: no lease waits anywhere the host cannot hurry.
    //
    // A `SurfaceLease` pins the native Surface, and RELEASED is published by the
    // last one going away, so wherever a lease waits, `migo_surface_begin_detach`
    // waits with it. It waited in two places: handed to the worker at spawn it was
    // owned across GPU bring-up -- measured at 33 ms on macOS and 5.7-41 s on the
    // iOS simulator, where ANGLE compiles its Metal shaders cold -- and carried
    // inside `RecreateOnscreen` it sat in a queue behind the same phase. Neither
    // pin bought anything: `CanvasManager` construction takes an `EglProvider` and
    // never names a window, and the candidate's liveness is re-checked at install
    // regardless.
    //
    // Three halves, and the ordering one alone is not enough -- either delivery
    // route reappearing would restore the pin while this file still looked right.
    let claim = RENDER_THREAD
        .find("surface_control.live_candidate()")
        .expect("the render thread must claim its candidate from the control plane");
    let init = RENDER_THREAD
        .find("CanvasManager::new_with_resource(")
        .expect("render initialization must construct CanvasManager");
    assert!(
        init < claim,
        "the candidate must be claimed after GPU initialization, not carried through it"
    );

    let spawn = RENDER_THREAD
        .split("pub fn spawn(")
        .nth(1)
        .expect("RenderThread::spawn must remain present");
    let signature = spawn
        .split(") -> EngineResult<Self> {")
        .next()
        .expect("RenderThread::spawn must have a signature");
    assert!(
        !code_only(signature).contains("SurfaceLease"),
        "RenderThread::spawn must take no Surface: the control plane is the only \
         route, so there is nowhere else for a pin to reappear"
    );

    // The second route that used to exist, and the reason the level is read rather
    // than consumed. `RecreateOnscreen` carried an owning lease, so a pre-ready
    // re-attach left the host's Surface pinned in a bounded queue behind the same
    // initialization -- unbounded, because `RenderService` gives up on the reply
    // after 500 ms and the lease stays queued regardless.
    let command = RENDER_CMD
        .split("    RecreateOnscreen {")
        .nth(1)
        .expect("the RecreateOnscreen command must remain present")
        .split("    },")
        .next()
        .expect("the RecreateOnscreen command must end");
    assert!(
        !code_only(command).contains("SurfaceLease"),
        "RecreateOnscreen must carry no Surface: it is the wake and the reply \
         channel for a level, not the delivery of a lease"
    );
    // It must still say *which* Surface, though. A request can outlive its own
    // candidate -- the reply times out after 500 ms while the request stays queued --
    // and one that could not name a generation would install whatever replaced it,
    // under its own stale presentation parameters, and on failure retire the
    // generation the host had just attached. Carrying its own lease used to make it
    // self-identifying; a generation does the same and pins nothing.
    // A publication, not a generation: `attach_or_update` reuses the live generation,
    // so a resize republishes one and two queued requests could name the same
    // generation -- letting the older match the newer's candidate.
    assert!(
        code_only(command).contains("revision: SurfaceCandidateRevision,"),
        "RecreateOnscreen must name the publication it is for"
    );
    assert!(
        !code_only(command).contains("SurfaceGeneration"),
        "a generation cannot identify a request: one is published more than once"
    );
    assert!(
        RENDER_THREAD.contains("surface_control.live_candidate_for(requested)"),
        "the recreate path must read the level for the generation it was asked \
         about, not whatever is published when it gets there"
    );
    assert!(
        RENDER_SERVICE.contains("self.surface_control.publish_candidate(lease.clone());"),
        "an update must publish through the control plane"
    );

    // And the control plane must actually be able to revoke what it parked.
    assert!(
        CONTROL.contains("candidate: Mutex<Option<(SurfaceCandidateRevision, SurfaceLease)>>"),
        "SurfaceControl must own the published candidate, paired with the publication \
         a request can name"
    );
    for retire in [
        "pub fn retire_current_and_request(&self)",
        "pub fn retire_generation_and_request(&self, expected: SurfaceGeneration)",
    ] {
        let body = CONTROL
            .split(retire)
            .nth(1)
            .unwrap_or_else(|| panic!("{retire} must remain present"))
            .split("\n    pub fn ")
            .next()
            .expect("the retirement body must end");
        assert!(
            body.contains("self.release_dead_candidate();"),
            "{retire} must revoke a published candidate it just made unusable"
        );
    }
}

#[test]
fn an_initial_surface_that_cannot_be_installed_is_reported_like_a_later_one() {
    // The property: the two install paths answer a genuine platform failure the
    // same way.
    //
    // They did not. An update that fails to install retires the generation and
    // reports the loss on the must-deliver control channel; an *initial* install
    // that fails only logged and set `gpu_caps`. That leaves nothing to observe:
    // the worker stays alive and running, so no channel closes and no reply
    // arrives, and the external-frame execution does not read `gpu_caps`. A host
    // whose Surface could not be used was therefore never told, and could not know
    // to attach another.
    //
    // Both arms are pinned, because "they agree" is the property and a check on one
    // of them cannot express it.
    let init = RENDER_THREAD
        .split("if let Some((revision, lease)) = claimed {")
        .nth(1)
        .expect("the initial install must remain present")
        .split("cm.publish_gpu_caps();")
        .next()
        .expect("the initial install must end before the caps publication");
    let update = RENDER_THREAD
        .split("let Some(lease) = surface_control.live_candidate_for(requested)")
        .nth(1)
        .expect("the update install must remain present")
        // Bounded at the next arm. Unbounded, this swept in the presentation path,
        // which retires by generation deliberately: it is asking about the Surface
        // already installed, not about a request that failed to install one.
        .split("\n                            other => {")
        .next()
        .expect("the update arm must end");

    for (arm, which) in [
        (init, "the initial install"),
        (update, "the update install"),
    ] {
        // `retire_published_surface`, not `retire_unexpected_surface`: a request whose
        // install failed must retire the publication it acted on, not everything
        // sharing its generation. A resize republishes the live generation, so the
        // generation-only form would revoke a valid replacement and report its loss to
        // a host that had just supplied it. That form remains for the presentation
        // path, which asks about what is installed rather than about a request.
        assert!(
            arm.contains("retire_published_surface("),
            "{which} must retire the publication it acted on and report the loss when \
             the platform genuinely refuses the Surface"
        );
        assert!(
            !code_only(arm).contains("retire_unexpected_surface("),
            "{which} must not retire by generation: a resize republishes the live one"
        );
        assert!(
            arm.contains("SurfaceLossReason::PlatformError"),
            "{which} must name the reason the host reads"
        );
    }

    // And the report must not cost a second pin. Naming what failed is what the
    // update path keeps a retained clone of the lease for; the initial path takes the
    // revision out of the read itself and the public generation before the lease
    // moves, both `Copy`.
    assert!(
        RENDER_THREAD.contains("if let Some((revision, lease)) = claimed {"),
        "the initial read must yield the publication with the lease, so naming it \
         later costs nothing"
    );
    assert!(
        init.contains("let public_generation = lease.public_generation();"),
        "the host-facing generation must be taken before the lease moves into the \
         install"
    );
    assert!(
        !code_only(init).contains("lease.clone()"),
        "the initial install must not retain a clone of the lease to report with: \
         that is the pin the candidate level exists to avoid"
    );
}

#[test]
fn a_surface_that_fails_at_swap_time_is_retired_by_generation_not_by_publication() {
    // The complement of the previous test, and it is not symmetry for its own sake:
    // three of the four retirement sites moved to the publication form, so the fourth
    // now looks like an oversight. Swept along with them it would be a regression, and
    // nothing here would have noticed -- the assertions above only say the *request*
    // paths must not use the generation form, which stays true when the generation form
    // has no callers left at all.
    //
    // What is dead at swap time is the native window, and every publication of one
    // generation is that same window: a resize rebuilds the descriptor over the
    // attachment's own resource. Retiring by the installed publication would therefore
    // retire nothing whenever a resize had superseded it -- gate still live over a dead
    // window, host never told, queued request free to install it. Proven in
    // `a_surface_found_unusable_while_presenting_must_retire_by_generation`.
    let presentation = RENDER_THREAD
        .split("} else if cm.is_surface_unavailable() {")
        .nth(1)
        .expect("the swap path must still classify an unusable Surface")
        .split("\n                                } else {")
        .next()
        .expect("that arm must end");

    assert!(
        presentation.contains("retire_unexpected_surface("),
        "a Surface the driver refused at swap time must be retired by generation: the \
         window is what failed, and a resize republishes that window"
    );
    assert!(
        !code_only(presentation).contains("retire_published_surface("),
        "the publication form must not be swept in here: superseded by a resize it \
         retires nothing, leaving a dead window live and the host untold"
    );
    assert!(
        presentation.contains("render_binding.generation()"),
        "and the generation must come from what is installed, not from what is \
         published: they differ exactly when this matters"
    );
}

#[test]
fn a_surface_the_host_took_back_is_cancellation_and_one_it_refused_is_a_loss() {
    // The property: the two ways an initial install can fail to install are told
    // apart, and each is answered with the thing that is true of it.
    //
    // A candidate the host retired between the read and the preflight is
    // cancellation. Nothing is reported, because the host is the party that retired
    // it -- it knows. What is left is the state a warm start begins in: capabilities
    // real, no Surface, waiting for the `UpdateSurface` that brings the next one.
    //
    // A candidate the *platform* refused is a loss. The host does not know, has no
    // other channel to learn it on, and needs to attach another -- so that arm
    // retires the generation and reports.
    //
    // Neither declares the GPU unusable; see
    // `gpu_caps_failed_means_the_gpu_did_not_come_up_and_nothing_else` for what that
    // conflation cost.
    let install = RENDER_THREAD
        .split("if let Some((revision, lease)) = claimed {")
        .nth(1)
        .expect("the initial install must remain present")
        .split("cm.publish_gpu_caps();")
        .next()
        .expect("the initial install must end before the caps publication");
    let cancelled_at = install
        .find("Err(SurfaceRecreateError::Binding(SurfaceBindingError::StaleGeneration))")
        .expect("a retired candidate must be matched separately from a refused one");
    let refused_at = install[cancelled_at..]
        .find("Err(error) => {")
        .map(|offset| cancelled_at + offset)
        .expect("a refused candidate must still have an arm");
    let cancelled = code_only(&install[cancelled_at..refused_at]);
    let refused = code_only(&install[refused_at..]);

    assert!(
        !cancelled.contains("retire_published_surface("),
        "a candidate the host retired must not be reported back to the host: it is \
         the party that retired it"
    );
    assert!(
        refused.contains("retire_published_surface("),
        "a candidate the platform refused must be reported: the host has no other \
         way to learn it, and needs to attach another"
    );
    assert!(
        !cancelled.contains("set_failed"),
        "cancellation must not declare the GPU unusable: the host took its own \
         Surface back and the capabilities were never in question"
    );
    // Neither does the refused arm, now that a loss reaches every host. It did until
    // Android could hear one -- see `a_refused_surface_does_not_declare_the_gpu_unusable`
    // for what that latch cost and what replaced it.
    assert!(
        !refused.contains("set_failed"),
        "a refused Surface says nothing about the GPU: the capabilities are real and \
         the latch made a recoverable failure permanent"
    );
}

#[test]
fn render_init_panic_wakes_gpu_join_as_failure() {
    let panic_handler = RENDER_THREAD
        .split("Err(panic_info) => {")
        .nth(1)
        .expect("render panic handler must remain present");
    assert!(panic_handler.contains("if !gpu_caps.is_ready()"));
    assert!(panic_handler.contains("gpu_caps.set_failed"));
}

#[test]
fn the_render_worker_names_every_way_it_stops() {
    // The property: a session that observes its renderer gone can say why.
    //
    // It could not before. The worker logged its reason and dropped its frame
    // clock sender; the external session logged "frame clock closed" and exited;
    // and the host, whose `on_error` exists for this, heard nothing. What it heard
    // instead was `attach` losing a race for a registry entry the exiting session
    // was removing -- about two runs in three on the iOS simulator, arriving as a
    // bare MIGO_ERROR_INTERNAL with no reason attached.
    //
    // Pinned in three parts, because each one alone is satisfiable while the
    // property is false.
    let body = RENDER_THREAD
        .split("std::panic::catch_unwind(std::panic::AssertUnwindSafe(")
        .nth(1)
        .expect("the render body must remain inside the panic barrier");

    // One: the body is typed, so an exit cannot decline to say which it was.
    assert!(
        body.contains("|| -> Result<(), EngineError> {"),
        "the render body must return a Result, so every exit names itself rather \
         than relying on whoever wrote it to remember"
    );

    // Two: the reason is published in exactly one place, at the tail. Two publish
    // sites is how a later exit comes to be reported by neither.
    let publishes = RENDER_THREAD
        .matches("render_exit.publish_failure(")
        .count();
    assert_eq!(
        publishes, 2,
        "the tail must be the only publisher: one site for a failed body and one \
         for a panic, and nothing anywhere else"
    );
    // Anchored on the barrier's own closing marker, not on `match result {`:
    // there are two of those in this file and `nth(1)` picked the wrong one, which
    // is the extraction-by-name-pattern mistake this suite exists to avoid.
    let tail = RENDER_THREAD
        .split("})); // end catch_unwind")
        .nth(1)
        .expect("the tail must classify the body's outcome");
    assert!(
        tail.contains("Ok(Ok(())) => {}"),
        "a body that ran and was asked to stop must record nothing"
    );
    assert!(
        tail.contains("Ok(Err(failure)) => render_exit.publish_failure(failure)"),
        "a body that failed must publish the failure it returned"
    );

    // Three: a panic publishes whatever `gpu_caps` says. A panic after the first
    // frame leaves that level reporting Ready, so gating the terminal reason on it
    // -- the way the caps write beside it is gated, correctly -- would lose exactly
    // the panics that happen once a session is running.
    let panic_arm = tail
        .split("Err(panic_info) => {")
        .nth(1)
        .expect("the panic arm must remain present");
    let caps_guard = panic_arm
        .find("if !gpu_caps.is_ready()")
        .expect("the caps write must stay guarded");
    let publish = panic_arm
        .find("render_exit.publish_failure(")
        .expect("a panic must publish a terminal reason");
    assert!(
        caps_guard < publish,
        "the guarded caps write comes first; the unguarded publish follows it"
    );
    let guarded = &panic_arm[caps_guard..publish];
    assert_eq!(
        guarded.matches('{').count(),
        guarded.matches('}').count(),
        "the publish must sit outside the readiness guard, not inside it"
    );
}

#[test]
fn a_stop_with_a_reason_is_distinguishable_from_one_without() {
    // The property: the loop cannot flatten a reason it was given.
    //
    // A direct `ReleaseOnscreen` that cannot prove the driver let go of the native
    // reference terminates the worker deliberately, and the EGL error is the only
    // account of why. Returning the same `Shutdown` a requested stop returns
    // discarded it, and the session then reported "the renderer stopped; it recorded
    // no reason" while the reason was in hand.
    let ctl = RENDER_THREAD
        .split("enum LoopCtl {")
        .nth(1)
        .expect("the loop control enum must remain present")
        .split('}')
        .next()
        .expect("the enum must end");
    assert!(
        code_only(ctl).contains("Failed(EngineError)"),
        "the loop control must be able to carry a reason, or a stop that has one \
         cannot say so"
    );
    assert!(
        RENDER_THREAD.contains("return LoopCtl::Failed(failure);"),
        "the release failure must return its reason rather than a bare stop"
    );

    // Every site that acts on a stop must act on both kinds. The count is derived,
    // so a sixth site added without the failing arm moves one number and not the
    // other -- though the compiler gets there first, since the match is exhaustive.
    let requested = RENDER_THREAD.matches("LoopCtl::Shutdown => {").count();
    let failed = RENDER_THREAD
        .matches("LoopCtl::Failed(failure) => {")
        .count();
    assert_eq!(
        requested, failed,
        "every loop site that handles a requested stop must handle a failing one: \
         {requested} handle the first, {failed} the second"
    );
    assert!(
        RENDER_THREAD.contains("return Err(failure);"),
        "a failing stop must leave the body as an Err, which is what the tail turns \
         into the published reason"
    );
}

#[test]
fn the_session_reports_the_reason_its_renderer_stopped() {
    // The other half: publishing is worth nothing if nobody reads it. The external
    // execution is the one with a `select!` arm that can see the frame clock close,
    // and that arm is where the host has to be told.
    let arm = EXTERNAL
        .split("timestamp = raf_rx.recv(raf_demand.session_ticket())")
        .nth(1)
        .expect("the external session must select on its frame clock")
        .split("\n            }")
        .next()
        .expect("the frame-clock arm must end");
    let closed = arm
        .find("None => {")
        .expect("a closed frame clock must still be handled");
    let closed = &arm[closed..];
    assert!(
        closed.contains("render_exit.failure()"),
        "the closed-clock branch must read why the worker stopped"
    );
    assert!(
        closed.contains("platform_for_error.notify_error("),
        "and must tell the host, which is the whole point of recording it"
    );
    assert!(
        closed.contains("\"the renderer stopped\""),
        "a worker that stopped without recording a reason must still be reported: \
         this branch is unreachable on a requested shutdown, so silence here would \
         only ever hide a real failure"
    );
}

#[test]
fn caller_ready_signal_stays_after_host_construction() {
    // Readiness is published by the shared runtime helper, which the embedded
    // body calls only after `Host::new` has returned. Keeping the order pinned
    // across the two files is the point: the helper moved, and a caller that
    // signalled ready before construction finished would turn a construction
    // failure into a hang rather than into a synchronous error.
    let construct = THREAD
        .find("Host::new(")
        .expect("host thread must construct Host");
    let publish = THREAD
        .find("create_runtime_before_ready(")
        .expect("host thread must publish readiness through the shared helper");
    assert!(
        construct < publish,
        "readiness must be published after Host construction, not before"
    );
    assert!(
        SESSION_THREAD.contains("ready_tx.send(())"),
        "the shared helper is what actually signals readiness"
    );
}

#[test]
fn every_startup_error_the_caller_sees_was_announced_exactly_once() {
    // The property: `spawn_session_thread` returns no `Err` the host has not
    // already heard about, and none it has heard about twice.
    //
    // It needs pinning because the two halves look interchangeable from either
    // end. A failure inside the thread announces its own specific reason and
    // *then* drops `ready_tx` -- and dropping `ready_tx` unsent is the only
    // thing the caller's handshake can observe. So a caller that reports what
    // the handshake told it delivers a second, vaguer callback for a failure
    // that already spoke, and the host cannot collapse the two: the notifier
    // posts every event independently. That is not hypothetical; the duplicate
    // existed, at the C boundary, and the two causes are distinguishable there
    // only by matching on the error message.
    let start = SESSION_THREAD
        .find("pub(crate) fn spawn_session_thread<Body>(")
        .expect("spawn_session_thread must remain present");
    let body = &SESSION_THREAD[start..];
    let end = body
        .find("pub(crate) struct StartedHost")
        .expect("spawn_session_thread must end before StartedHost");
    let body = &body[..end];

    let spawn_at = body
        .find("let spawn_result = thread::Builder::new()")
        .expect("the thread spawn must remain present");
    let join_at = body
        .find("let join = match spawn_result {")
        .expect("the spawn outcome must still be matched");
    let host_at = body
        .find("let host = HostThread::new(id, join);")
        .expect("the spawn-failure arm must end at the HostThread it could not build");
    let handshake_at = body
        .find("if ready_rx.recv().is_err() {")
        .expect("the cold-start handshake must remain present");

    // The two regions where no thread can be holding `ready_tx`: before the
    // spawn statement, and the arm where the spawn itself failed and dropped the
    // closure without running it. Derived by pairing counts rather than by
    // listing the exits, so an exit added without a report moves one count and
    // not the other.
    for (region, what) in [
        (&body[..spawn_at], "before the thread is spawned"),
        (&body[join_at..host_at], "when the spawn itself fails"),
    ] {
        let built = region.matches("EngineError::new(").count();
        let announced = region.matches("report_unspawned_failure(").count();
        assert!(
            built > 0,
            "the region {what} must still have a failure to announce"
        );
        assert_eq!(
            built, announced,
            "every error built {what} must be announced, because no thread exists \
             to announce it: {built} built, {announced} announced"
        );
    }

    // And the mirror. Past the handshake the thread has already spoken for every
    // way it can fail, including a panic, which the barrier reports.
    let after_handshake = &body[handshake_at..];
    assert!(
        !code_only(after_handshake).contains("report_unspawned_failure("),
        "the handshake observes a failure that already reported; announcing it \
         again is the duplicate this test exists to prevent"
    );
    assert!(
        !code_only(after_handshake).contains("notify_error("),
        "the handshake must not report through any route"
    );

    // The pairing above is only meaningful if the helper is the sole reporter in
    // those two regions -- a bare `notify_error` there would satisfy no count.
    assert!(
        !code_only(&body[..spawn_at]).contains("notify_error("),
        "pre-spawn reporting must go through report_unspawned_failure"
    );
    assert!(
        !code_only(&body[join_at..host_at]).contains("notify_error("),
        "spawn-failure reporting must go through report_unspawned_failure"
    );
}
