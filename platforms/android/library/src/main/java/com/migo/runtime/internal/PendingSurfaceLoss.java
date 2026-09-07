package com.migo.runtime.internal;

import java.util.concurrent.ConcurrentHashMap;
import java.util.function.IntPredicate;

/**
 * Surface-loss reports that arrived before their session could receive them.
 *
 * <p>{@code NativeMethods.init} spawns the render thread and only then does the
 * {@code GameSession} constructor register, so a session started with a Surface can have
 * that Surface refused inside the gap. Dropping the report there costs the app the only
 * signal telling it to attach a replacement.
 *
 * <p>A class of its own, rather than two lines inside {@code NativeExports}, because the
 * claim it makes is <em>exactly one</em> delivery and that claim has been wrong twice: the
 * first version dropped the report entirely, the second recreated entries for closed ids,
 * and the third let registration slip between the lookup and the insert. {@code
 * NativeExports} holds a {@code Handler} on the main {@code Looper}, so nothing in it can
 * be constructed by a JVM unit test; here the arbitration can be driven directly and its
 * interleavings actually run.
 *
 * <p>The arbitration: whoever {@link #drain} answers with the report is the one that
 * delivers it. Both sides call it — the reporter after inserting and rechecking, the
 * registration on arrival — and a {@code ConcurrentHashMap} removal answers exactly one of
 * them. So the two cannot both deliver, and cannot both decline.
 */
final class PendingSurfaceLoss {

    /**
     * Bounded by the number of sessions that were reported a loss before registering, and
     * emptied by {@link #drain} or {@link #discard}. Retention is refused for an id that
     * has already been closed, which is what stops an entry nobody will ever drain.
     */
    private final ConcurrentHashMap<Integer, long[]> retained = new ConcurrentHashMap<>();

    /**
     * Retain a report for a session that has not registered yet, or hand it straight back
     * if registration overtook this call.
     *
     * <p>{@code registered} is re-consulted after the insert, and that is the whole point:
     * a caller that only checked before it inserted could have registration -- and its
     * drain -- run in between, leaving an entry that nothing would ever replay. Rechecking
     * closes it because the two orderings are exhaustive. If the drain ran after the
     * insert it takes the report and this returns {@code null}; if it ran before, the
     * recheck sees the session and takes the report here.
     *
     * @return the report to deliver now, or {@code null} to leave it retained
     */
    long[] retainOrTakeBack(int sessionId, long generation, int reason, IntPredicate registered) {
        // First-wins, as at the C boundary: a second loss inside the window is for a
        // Surface whose predecessor's loss has not been delivered yet, so overwriting
        // would skip the one the app needs.
        retained.putIfAbsent(sessionId, new long[] {generation, reason});
        return registered.test(sessionId) ? drain(sessionId) : null;
    }

    /** Take whatever is retained for a session, if anything. */
    long[] drain(int sessionId) {
        return retained.remove(sessionId);
    }

    /**
     * Forget a session's report because nothing will ever receive it.
     *
     * <p>Distinct from {@link #drain} only in that the caller does not want the value;
     * naming the two apart is what keeps a teardown from reading like a delivery.
     */
    void discard(int sessionId) {
        retained.remove(sessionId);
    }

    /** For tests: whether anything is retained at all. */
    boolean isEmpty() {
        return retained.isEmpty();
    }
}
