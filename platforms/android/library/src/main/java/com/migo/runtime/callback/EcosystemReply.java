package com.migo.runtime.callback;

import java.util.Map;

/**
 * Content's answer to an event posted with
 * {@link com.migo.runtime.GameSession#postEcosystemEvent(String, Map, EcosystemReply)}:
 * what an {@code onCopyUrl} or {@code onHandoff} listener returned, or each
 * {@code resolve()} of the {@code onNeedPrivacyAuthorization} listener.
 * <p>
 * Every event posted with a reply is answered, and its last answer has
 * {@code done} set -- including when nothing in the game listens, or the game's
 * runtime restarts first, both of which answer {@code null}. Answers arrive on
 * the runtime's host thread.
 */
public interface EcosystemReply {

    /**
     * @param data what content answered, as an immutable tree of {@code Map},
     *             {@code List}, {@code String}, {@code Number}, {@code Boolean}
     *             and {@code null}; null when no listener answered
     * @param done whether this is the last answer for the event
     */
    void onReply(Map<String, Object> data, boolean done);
}
