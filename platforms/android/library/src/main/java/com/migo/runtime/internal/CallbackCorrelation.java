package com.migo.runtime.internal;

import org.json.JSONException;
import org.json.JSONObject;

/**
 * The request id a deferred call carries, and how a result echoes it back.
 *
 * <p>The runtime allocates one id per deferred request from a space that
 * outlives the JavaScript isolate, sends it in the request JSON, and matches
 * the reply by it and by nothing else: a reply that omits the id names no
 * request and is discarded. (It once settled the <em>oldest</em> pending
 * request instead, which was wrong whenever two were in flight — two
 * concurrent Sessions, or one game calling the same API twice.)
 *
 * <p>Two rules live here rather than in each manager, because a second copy of
 * either is a second answer to who a result belongs to:
 *
 * <ul>
 *   <li>An id is a positive 32-bit integer. The runtime rejects anything else,
 *       so writing a value it will reject is the same as writing nothing —
 *       except that it also loses the fallback.
 *   <li><b>Absent stays absent.</b> A request that carried no id must get a
 *       reply with no {@code requestId} key at all, rather than an id the
 *       runtime never issued.
 * </ul>
 */
public final class CallbackCorrelation {
    /** No id: the request carried none, so no reply may claim one. */
    public static final int ABSENT = 0;

    private CallbackCorrelation() {}

    /** The id in a request's options JSON, or {@link #ABSENT}. */
    public static int requestIdOf(String optionsJson) {
        if (optionsJson == null || optionsJson.isEmpty()) return ABSENT;
        try {
            return requestIdOf(new JSONObject(optionsJson));
        } catch (JSONException malformed) {
            return ABSENT;
        }
    }

    /** The id in already-parsed options, or {@link #ABSENT}. */
    public static int requestIdOf(JSONObject options) {
        if (options == null) return ABSENT;
        // The type is checked before the value, and `optInt` is deliberately not
        // used: it *truncates*, so `1.5` reads back as the id 1 and a reply would
        // settle whichever request holds 1. Same shape as the runtime's own
        // parser, where `Number(true)` is 1 for the same reason.
        Object raw = options.opt("requestId");
        long id;
        if (raw instanceof Integer) {
            id = (Integer) raw;
        } else if (raw instanceof Long) {
            id = (Long) raw;
        } else {
            return ABSENT;
        }
        return (id > 0 && id <= Integer.MAX_VALUE) ? (int) id : ABSENT;
    }

    /** Echo {@code requestId} onto a result being built, when there is one. */
    public static void stamp(JSONObject result, int requestId) throws JSONException {
        if (requestId > ABSENT) {
            result.put("requestId", requestId);
        }
    }

    /**
     * A failure result that answers the request it was given.
     *
     * <p>Built with {@link JSONObject} rather than string concatenation so the
     * escaping is the library's problem, not a hand-written one that has to be
     * right in every manager.
     */
    public static String failure(int requestId, String apiName, String reason) {
        try {
            JSONObject error = new JSONObject();
            error.put("error", apiName + ":fail " + (reason == null ? "unknown error" : reason));
            stamp(error, requestId);
            return error.toString();
        } catch (JSONException impossible) {
            // Only thrown for a null key or a non-finite number, neither of
            // which can occur above. Fall back to a reply the runtime can still
            // route rather than to nothing at all.
            return "{\"error\":\"" + apiName + ":fail result serialisation failed\"}";
        }
    }
}
