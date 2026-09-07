//! What a Session's teardown may and may not take with it.
//!
//! Pinned against the source rather than exercised, and the reason is worth
//! stating: proving the behaviour would need two live `Host`s, and a `Host` needs a
//! surface and a GPU. The property here is structural anyway -- "this call must not
//! be here" -- which is what a source assertion can honestly express. It is not
//! standing in for a behavioural guarantee; the behaviour that makes the absence
//! safe is covered elsewhere, by the variant-token invalidation tests in
//! `runtime-v8`'s image module.

const HOST: &str = include_str!("../host.rs");

/// Everything from the start of `Drop for Host` to the end of the impl.
fn host_drop_body() -> &'static str {
    let start = HOST
        .find("impl Drop for Host")
        .expect("Host must still implement Drop");
    let rest = &HOST[start..];
    let end = rest
        .find("\nimpl Host {")
        .expect("the Drop impl must be followed by the inherent impl");
    &rest[..end]
}

/// The decoded-image cache is shared across Sessions on purpose: its entries are
/// context-independent RGBA and its key carries the resource's real identity, so two
/// games loading one asset hold one copy. Clearing it on a Session's teardown threw
/// away every *live* Session's images -- a defect inherited from a single-session
/// world, where the comment above the call reasoned about "the next session".
#[test]
fn tearing_down_a_session_does_not_clear_the_shared_decoded_image_cache() {
    let body = host_drop_body();
    assert!(
        !body.contains("global_cache().clear()"),
        "Host::drop must not clear the process-wide decoded-image cache: it holds \
         other live sessions' entries. A changed file already yields a different \
         cache key, so clearing buys no invalidation."
    );
}

/// The restart path had the same call for the same reason, and the same problem: a
/// restart of one game must not discard another game's decoded images.
#[test]
fn restarting_a_session_does_not_clear_the_shared_decoded_image_cache() {
    // The *function*, not the first mention of the name: `on_restart` appears in a
    // doc comment far above `impl Drop for Host`, so searching for the bare name
    // gave a window that swallowed the whole Drop body and this test then failed for
    // Drop's defect rather than its own.
    let start = HOST
        .find("async fn on_restart(")
        .expect("on_restart must remain present");
    let restart = &HOST[start..];
    // Bounded by the next method at member indent, or the end of the file when
    // `on_restart` is the last one -- which it currently is, and an `expect` here
    // panicked instead of asserting.
    let end = [
        "\n    async fn ",
        "\n    fn ",
        "\n    pub fn ",
        "\n    pub(crate) fn ",
    ]
    .iter()
    .filter_map(|marker| restart[1..].find(marker).map(|offset| offset + 1))
    .min()
    .unwrap_or(restart.len());
    assert!(
        !restart[..end].contains("global_cache().clear()"),
        "the restart path must not clear the process-wide decoded-image cache"
    );
}

/// The GPU alias table is the opposite case and must still be dropped, now per
/// Session: its entries name textures in an EGL context that is gone, so leaving them
/// reachable is worse than over-clearing. This test exists so the two caches are not
/// confused for each other again -- they look alike and want opposite treatment.
///
/// Only the presence of the call is pinned here. That it drops *this* Session's table
/// and no other is no longer a source property to assert: the function takes a host
/// id, so a process-wide drop is not expressible. Deleting the call outright still
/// compiles, which is what this catches; the per-session behaviour is covered by the
/// isolation tests beside the registry itself in `runtime-v8`.
#[test]
fn tearing_down_a_session_still_drops_its_gpu_image_aliases() {
    let body = host_drop_body();
    assert!(
        body.contains("unregister_image_cache(self.id)"),
        "Host::drop must still drop this session's GPU image aliases; their texture \
         ids are meaningless once its EGL context is gone"
    );
}

/// The launch path's body, from its signature to the start of the next method.
///
/// `launch_content` and not `on_evaluate_module`: the latter is now the thin
/// announcement pairing around it, so the work this file asserts about lives one
/// level down.
fn launch_content_body() -> &'static str {
    let start = HOST
        .find("async fn launch_content(")
        .expect("the launch path must remain present");
    let rest = &HOST[start..];
    // Bounded by whichever member-indented method declaration comes first, so a
    // reorder that puts an `async fn` next does not widen the window.
    let end = [
        "\n    fn ",
        "\n    async fn ",
        "\n    pub fn ",
        "\n    pub(crate) fn ",
    ]
    .iter()
    .filter_map(|marker| rest[1..].find(marker).map(|offset| offset + 1))
    .min()
    .expect("launch_content must end before the next method");
    &rest[..end]
}

/// `/tmp` is documented to start empty for a session and not to outlive it.
/// Android's Java SDK sweeps it; every other host reaches the engine through
/// this crate and had no equivalent, so `GamePaths::clean_temp` sat with no
/// caller and each session's `tmp/{id}` subtree leaked under the cache root.
///
/// The owner is a `Host` field so teardown is RAII on every exit path,
/// including a panic — the same reason the guard is a field and not a line in
/// `Drop`. Deleting the field is what this catches; the clear/remove behaviour
/// itself is covered by `session_temp`'s own unit tests.
#[test]
fn a_sessions_temp_directory_is_owned_for_removal_at_teardown() {
    let struct_start = HOST
        .find("pub(crate) struct Host {")
        .expect("Host struct must remain present");
    let struct_body = &HOST[struct_start
        ..HOST[struct_start..]
            .find("\n}")
            .map(|offset| struct_start + offset)
            .expect("Host struct must close")];
    assert!(
        struct_body.contains("session_temp: Option<SessionTemp>"),
        "Host must own a SessionTemp so a session's /tmp subtree is removed when \
         the Host drops, on every teardown path"
    );
}

/// A runtime restart re-enters the launch path with the same session id and
/// game id, and `/tmp` must survive it — a restart is the same session. So the
/// guard is created once, gated on its own absence, not rebuilt each eval.
#[test]
fn the_temp_directory_is_prepared_once_and_survives_a_restart() {
    let body = launch_content_body();
    let prepare = body
        .find("SessionTemp::prepare(")
        .expect("the first module eval must prepare this session's /tmp");
    let gate = body
        .find("self.session_temp.is_none()")
        .expect("temp preparation must be gated so a restart keeps its /tmp");
    assert!(
        gate < prepare,
        "the is_none() gate must guard the SessionTemp::prepare call"
    );
}
