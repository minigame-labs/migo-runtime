package com.migo.runtime.callback;

/**
 * The channel a {@link PermissionHandler} answers on, one per session: the one
 * {@link PermissionHandler#requestScope} is handed is the one
 * {@link com.migo.runtime.GameSession#getPermissionSink} returns.
 * <p>
 * Two separate things, because they answer different questions:
 * {@link #setScope} is the standing decision {@code migo.getSetting()} reports
 * and every capability call is checked against; {@link #resolveRequest} settles
 * one pending {@code migo.authorize()}.
 *
 * <h2>Threading</h2>
 * Every method is safe from any thread. That is the point: deciding usually
 * means showing a dialog and answering on whatever thread its callback arrives
 * on.
 *
 * <p>Calls for a session that has ended are ignored, so you do not have to race
 * teardown to stay correct.
 */
public interface PermissionSink {

    /**
     * Set the standing decision for one scope.
     * <p>
     * Call this to seed a session from your own records, and again whenever a
     * decision changes — including a revocation the user makes in system
     * settings while the game is running. Decisions set before the game starts
     * are the ones it starts with. The runtime reads your latest answer
     * rather than a snapshot taken at start-up, so a revocation takes effect on
     * the next capability call.
     * <p>
     * A scope you never set reads as "not decided", which is distinct from
     * denied: content may still ask about it, and {@code migo.authorize()} is how.
     *
     * @param scope   platform scope name, e.g. {@code "scope.camera"}
     * @param granted whether the game may use it
     */
    void setScope(String scope, boolean granted);

    /**
     * Settle one pending {@code migo.authorize()} call.
     * <p>
     * Does not by itself change the standing decision — call {@link #setScope}
     * for that. Granting a request without recording it would let the game act
     * once and then find the capability refused, which reads as a bug in your
     * app rather than as a permission model.
     *
     * @param requestId the id passed to
     *                  {@link PermissionHandler#requestScope}
     * @param granted   whether the request was allowed
     */
    void resolveRequest(int requestId, boolean granted);

    /**
     * Settle one pending {@code migo.authorize()} call as failed.
     * <p>
     * For "could not ask" — no activity, a dialog that could not be shown — as
     * opposed to "asked and refused", which is
     * {@code resolveRequest(requestId, false)}. Content distinguishes the two:
     * a refusal sends the user to {@code migo.openSetting()}, an error is worth
     * retrying.
     *
     * @param requestId the id passed to
     *                  {@link PermissionHandler#requestScope}
     * @param errMsg    human-readable reason; must not be null
     */
    void failRequest(int requestId, String errMsg);
}
