package com.migo.runtime.callback;

/**
 * Host-provided permission handler: you decide what a game may do.
 * <p>
 * Register via
 * {@link com.migo.runtime.GameSession#setPermissionHandler(PermissionHandler)}
 * before the game starts.
 *
 * <h2>Why this is yours and not the runtime's</h2>
 * A mainstream mini-game platform is both the runtime and the host, so it owns the user relationship,
 * the consent dialog, and the record of what each mini-game was granted. Migo
 * runs inside your app and owns none of those. What the common mini-game platform merges, this splits: the
 * game declares its scopes in {@code game.json}, <em>you</em> decide, and the
 * runtime only carries the question and the answer.
 *
 * <h2>Without a handler</h2>
 * Every scope is denied. {@code migo.getSetting()} reports nothing granted and
 * capability calls fail with {@code auth deny}.
 * <p>
 * That is deliberate, and it is the same answer the ad bridge gives when no ad
 * handler is installed. A runtime that grants a capability nobody approved is
 * the same defect as one that pays out an advert nobody watched. It matters
 * most in exactly the case this SDK is built for: your app holds the camera
 * permission for its own features, and without this handler a third-party game
 * would reach the camera under your grant with nobody ever asked whether that
 * game should have it.
 *
 * <h2>Contract</h2>
 * <ul>
 *   <li>Seed the session's decisions with {@link PermissionSink#setScope} —
 *       from your own records, before or as the game starts, on the sink
 *       {@link com.migo.runtime.GameSession#getPermissionSink} returns. Scopes you
 *       never set read as "not decided", which content may still ask about.</li>
 *   <li>{@link #requestScope} must eventually answer, by calling
 *       {@link PermissionSink#resolveRequest}. Content is awaiting it.</li>
 *   <li>Call {@link PermissionSink#setScope} again whenever a decision changes
 *       — including a revocation made in system settings while the game runs,
 *       when no request is there to hand you a sink.
 *       The runtime reads your latest answer, it does not cache a snapshot.</li>
 *   <li>Calls arrive on the runtime's host thread; the sink is safe from any
 *       thread, which is what you need to show a dialog and answer later.</li>
 * </ul>
 */
public interface PermissionHandler {

    /**
     * Decide a scope, prompting the user if you see fit.
     * <p>
     * The common mini-game platform does not re-prompt after a refusal: content that was denied is meant
     * to be sent to {@code migo.openSetting()} instead. If you have already been
     * refused, answering {@code false} straight away preserves that behaviour;
     * prompting again on every call does not.
     *
     * @param requestId correlates the answer; pass it back to
     *                  {@link PermissionSink#resolveRequest}
     * @param scope     platform scope name, e.g. {@code "scope.camera"}
     * @param desc      the reason text the game declared in {@code game.json},
     *                  empty when it declared none. Shown to the user; a prompt
     *                  without it cannot say why the game is asking
     * @param sink      channel for the answer
     */
    void requestScope(int requestId, String scope, String desc, PermissionSink sink);
}
