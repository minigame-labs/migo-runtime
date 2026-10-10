package com.migo.runtime.internal;

import java.util.Collection;
import java.util.Collections;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicLong;

/**
 * The host callbacks waiting for content's answer to something the host posted,
 * per session, by reply id.
 *
 * <p>Ids are process-wide, so an answer can never reach another session's
 * callback. A callback is held until its last answer, until the runtime that
 * would answer is gone ({@link #abandon}), or until the session ends
 * ({@link #forget}).
 *
 * @param <C> the callback type
 */
final class ReplyRegistry<C> {

    private final AtomicLong next = new AtomicLong(1);
    private final ConcurrentHashMap<Integer, ConcurrentHashMap<Long, C>> pending =
            new ConcurrentHashMap<>();

    /** A reply id no session has used. */
    long newId() {
        return next.getAndIncrement();
    }

    /** Hold {@code callback} until reply {@code replyId}'s last answer. */
    void put(int sessionId, long replyId, C callback) {
        pending.computeIfAbsent(sessionId, id -> new ConcurrentHashMap<>()).put(replyId, callback);
    }

    /**
     * The callback waiting for {@code replyId}, or null; released when
     * {@code done}, so a last answer is handed over exactly once.
     */
    C take(int sessionId, long replyId, boolean done) {
        Map<Long, C> waiting = pending.get(sessionId);
        if (waiting == null) return null;
        return done ? waiting.remove(replyId) : waiting.get(replyId);
    }

    /** Every callback the session still holds, released: nothing will answer them. */
    Collection<C> abandon(int sessionId) {
        Map<Long, C> waiting = pending.remove(sessionId);
        return waiting != null ? waiting.values() : Collections.emptyList();
    }

    /** Release the session's callbacks without answering them. */
    void forget(int sessionId) {
        pending.remove(sessionId);
    }
}
