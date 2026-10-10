package com.migo.runtime.internal;

import com.migo.runtime.callback.EcosystemReply;

import org.json.JSONException;
import org.json.JSONObject;

import java.nio.charset.StandardCharsets;
import java.util.HashMap;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;

/**
 * The host's ecosystem, per session: the replies its posted events wait for and
 * the values its synchronous getters answer with.
 *
 * <p>Apart from {@code NativeExports}, which holds {@code android.*} statics, so
 * the routing and the bounds are reachable from a host-JVM unit test. The bounds
 * are the C ABI's ({@code migo_session_set_ecosystem_value}), so a host behaves
 * the same through either SDK.
 */
final class EcosystemBridge {

    private EcosystemBridge() {}

    /** Where a posted event goes: {@code NativeMethods.onEcosystemEvent}. */
    interface EventChannel {
        void deliver(int sessionId, String eventJson);
    }

    /** The longest API or event name. */
    static final int NAME_MAX = 64;
    /** How many getters one session may hold values for. */
    static final int VALUES_MAX = 64;
    /** The largest event or value, in UTF-8 bytes. */
    static final int PAYLOAD_MAX_BYTES = 1 << 20;

    private static final ReplyRegistry<EcosystemReply> sReplies = new ReplyRegistry<>();
    /** Guarded by the inner map itself. */
    private static final ConcurrentHashMap<Integer, Map<String, String>> sValues =
            new ConcurrentHashMap<>();

    /**
     * An API's name, or an object member's as {@code Class.member}: ASCII letters
     * with at most one dot between two of them, at most {@link #NAME_MAX} in all.
     */
    static boolean isApiName(String name) {
        if (name == null || name.isEmpty() || name.length() > NAME_MAX) return false;
        int dot = -1;
        for (int index = 0; index < name.length(); index++) {
            char c = name.charAt(index);
            if (c == '.') {
                if (dot != -1 || index == 0 || index == name.length() - 1) return false;
                dot = index;
            } else if (!((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z'))) {
                return false;
            }
        }
        return true;
    }

    /**
     * Post {@code name} with {@code data} to content, waiting for its answer on
     * {@code reply} when there is one.
     *
     * @throws IllegalArgumentException for a name that is not an API name, data
     *         that is not a JSON tree, or an event larger than
     *         {@link #PAYLOAD_MAX_BYTES}
     */
    static void post(
            int sessionId,
            String name,
            Map<String, ?> data,
            EcosystemReply reply,
            EventChannel channel) {
        if (!isApiName(name)) {
            throw new IllegalArgumentException("not an event name: " + name);
        }
        long replyId = reply != null ? sReplies.newId() : 0;
        String json;
        try {
            JSONObject event = new JSONObject();
            event.put("name", name);
            event.put("data", data != null ? HostDelegation.jsonValue(data) : new JSONObject());
            if (reply != null) event.put("replyId", replyId);
            json = event.toString();
        } catch (JSONException unserialisable) {
            throw new IllegalArgumentException(
                    "event data is not a JSON tree: " + unserialisable.getMessage());
        }
        if (json.getBytes(StandardCharsets.UTF_8).length > PAYLOAD_MAX_BYTES) {
            throw new IllegalArgumentException("event larger than " + PAYLOAD_MAX_BYTES + " bytes");
        }
        if (reply != null) sReplies.put(sessionId, replyId, reply);
        channel.deliver(sessionId, json);
    }

    /** {@code {"replyId", "data", "done"}} from content: hand it to the reply waiting for it. */
    static void reply(int sessionId, String replyJson) {
        JSONObject answer = HostDelegation.options(replyJson);
        Object raw = answer.opt("replyId");
        if (!(raw instanceof Integer || raw instanceof Long)) return;
        long replyId = ((Number) raw).longValue();
        boolean done = answer.optBoolean("done", true);
        EcosystemReply waiting = sReplies.take(sessionId, replyId, done);
        if (waiting == null) return;
        JSONObject data = answer.optJSONObject("data");
        waiting.onReply(data != null ? HostDelegation.immutableMap(data) : null, done);
    }

    /**
     * Answer every reply the session still waits for as nobody will: the runtime
     * that would have is gone. Each is answered even if one throws; the first
     * exception is rethrown after.
     */
    static void abandonReplies(int sessionId) {
        RuntimeException first = null;
        for (EcosystemReply waiting : sReplies.abandon(sessionId)) {
            try {
                waiting.onReply(null, true);
            } catch (RuntimeException thrown) {
                if (first == null) first = thrown;
            }
        }
        if (first != null) throw first;
    }

    /**
     * Report what the getter {@code name} answers with; null withdraws it.
     *
     * @throws IllegalArgumentException for a name that is not an API name, a value
     *         that is not a JSON tree or is larger than {@link #PAYLOAD_MAX_BYTES},
     *         or a {@link #VALUES_MAX}th-and-one name
     */
    static void setValue(int sessionId, String name, Object value) {
        if (!isApiName(name)) {
            throw new IllegalArgumentException("not a getter name: " + name);
        }
        Map<String, String> values = sValues.computeIfAbsent(sessionId, id -> new HashMap<>());
        if (value == null) {
            synchronized (values) {
                values.remove(name);
            }
            return;
        }
        String json;
        try {
            Object tree = HostDelegation.jsonValue(value);
            json = tree instanceof String ? JSONObject.quote((String) tree) : tree.toString();
        } catch (JSONException unserialisable) {
            throw new IllegalArgumentException(
                    "value is not a JSON tree: " + unserialisable.getMessage());
        }
        if (json.getBytes(StandardCharsets.UTF_8).length > PAYLOAD_MAX_BYTES) {
            throw new IllegalArgumentException("value larger than " + PAYLOAD_MAX_BYTES + " bytes");
        }
        synchronized (values) {
            if (!values.containsKey(name) && values.size() >= VALUES_MAX) {
                throw new IllegalArgumentException("more than " + VALUES_MAX + " values");
            }
            values.put(name, json);
        }
    }

    /** The JSON the getter {@code name} answers with, empty when none was reported. */
    static String value(int sessionId, String name) {
        Map<String, String> values = sValues.get(sessionId);
        if (values == null) return "";
        synchronized (values) {
            String json = values.get(name);
            return json != null ? json : "";
        }
    }

    /** Forget the session: its pending replies are dropped, not answered. */
    static void forget(int sessionId) {
        sReplies.forget(sessionId);
        sValues.remove(sessionId);
    }
}
