package com.migo.runtime.internal;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertThrows;

import com.migo.runtime.callback.EcosystemHandler;

import org.json.JSONObject;
import org.junit.After;
import org.junit.Test;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * The host's ecosystem through the Java SDK: events reach content and its answers
 * reach the host that waits for them, values answer the getters, and results
 * carry the tree a handler hands back.
 */
public final class EcosystemBridgeTest {

    private static final int SESSION = 4242;

    @After
    public void forget() {
        EcosystemBridge.forget(SESSION);
    }

    @Test
    public void an_event_carries_a_reply_id_and_its_answers_reach_the_reply_until_done()
            throws Exception {
        List<String> posted = new ArrayList<>();
        List<Object> answers = new ArrayList<>();
        Map<String, Object> data = new LinkedHashMap<>();
        data.put("referrer", "getUserInfo");
        EcosystemBridge.post(SESSION, "onNeedPrivacyAuthorization", data,
                (answer, done) -> answers.add(Arrays.asList(answer, done)),
                (session, json) -> posted.add(json));

        JSONObject event = new JSONObject(posted.get(0));
        assertEquals("onNeedPrivacyAuthorization", event.getString("name"));
        assertEquals("getUserInfo", event.getJSONObject("data").getString("referrer"));
        long replyId = event.getLong("replyId");

        EcosystemBridge.reply(SESSION, "{\"replyId\":" + replyId
                + ",\"data\":{\"event\":\"exposureAuthorization\"},\"done\":false}");
        EcosystemBridge.reply(SESSION, "{\"replyId\":" + replyId
                + ",\"data\":{\"event\":\"agree\"},\"done\":true}");
        EcosystemBridge.reply(SESSION, "{\"replyId\":" + replyId
                + ",\"data\":{\"event\":\"disagree\"},\"done\":true}");

        assertEquals(Arrays.asList(
                Arrays.asList(Collections.singletonMap("event", "exposureAuthorization"), false),
                Arrays.asList(Collections.singletonMap("event", "agree"), true)), answers);
    }

    @Test
    public void a_restart_answers_every_reply_still_waiting() {
        List<Object> answers = new ArrayList<>();
        EcosystemBridge.post(SESSION, "onCopyUrl", null,
                (answer, done) -> answers.add(Arrays.asList(answer, done)),
                (session, json) -> { });
        EcosystemBridge.abandonReplies(SESSION);
        EcosystemBridge.abandonReplies(SESSION);
        assertEquals(Collections.singletonList(Arrays.asList(null, true)), answers);
    }

    @Test
    public void an_event_without_a_reply_names_none_and_bad_input_is_refused() throws Exception {
        List<String> posted = new ArrayList<>();
        EcosystemBridge.post(SESSION, "onVoIPChatStateChanged", null, null,
                (session, json) -> posted.add(json));
        JSONObject event = new JSONObject(posted.get(0));
        assertFalse(event.has("replyId"));
        assertEquals(0, event.getJSONObject("data").length());

        assertThrows(IllegalArgumentException.class, () -> EcosystemBridge.post(
                SESSION, "on-copy", null, null, (session, json) -> { }));
        assertThrows(IllegalArgumentException.class, () -> EcosystemBridge.post(
                SESSION, "onCopyUrl", Collections.singletonMap("n", Double.NaN), null,
                (session, json) -> { }));
        assertThrows(IllegalArgumentException.class, () -> EcosystemBridge.post(
                SESSION, "onCopyUrl", Collections.singletonMap("o", new Object()), null,
                (session, json) -> { }));
    }

    @Test
    public void a_value_is_kept_as_json_and_withdrawn_by_null() {
        Map<String, Object> config = new LinkedHashMap<>();
        config.put("level", 3);
        config.put("tags", Arrays.asList("a", "b"));
        EcosystemBridge.setValue(SESSION, "getExtConfigSync", config);
        EcosystemBridge.setValue(SESSION, "isChatTool", true);
        EcosystemBridge.setValue(SESSION, "getExptInfoSync", "x\"y");

        assertEquals("{\"level\":3,\"tags\":[\"a\",\"b\"]}",
                EcosystemBridge.value(SESSION, "getExtConfigSync"));
        assertEquals("true", EcosystemBridge.value(SESSION, "isChatTool"));
        assertEquals("\"x\\\"y\"", EcosystemBridge.value(SESSION, "getExptInfoSync"));

        EcosystemBridge.setValue(SESSION, "isChatTool", null);
        assertEquals("", EcosystemBridge.value(SESSION, "isChatTool"));
        assertEquals("", EcosystemBridge.value(SESSION + 1, "getExtConfigSync"));
        assertThrows(IllegalArgumentException.class,
                () -> EcosystemBridge.setValue(SESSION, "", true));
    }

    @Test
    public void values_are_bounded_per_session() {
        for (int index = 0; index < EcosystemBridge.VALUES_MAX; index++) {
            EcosystemBridge.setValue(SESSION, "getter" + letters(index), index);
        }
        EcosystemBridge.setValue(SESSION, "getter" + letters(0), "replaced");
        assertThrows(IllegalArgumentException.class,
                () -> EcosystemBridge.setValue(SESSION, "oneTooMany", 1));
    }

    @Test
    public void a_request_and_its_result_carry_trees() throws Exception {
        EcosystemHandler.Request request = HostDelegation.ecosystemRequest(new JSONObject(
                "{\"requestId\":5,\"api\":\"getGroupCloudStorage\","
                        + "\"options\":{\"keyList\":[\"score\"]}}"));
        assertEquals("getGroupCloudStorage", request.api);
        assertEquals(Collections.singletonList("score"), request.options.get("keyList"));

        List<String> delivered = new ArrayList<>();
        HostDelegation.Settlement settlement = new HostDelegation.Settlement(
                SESSION, 5, (session, json) -> delivered.add(json), () -> false);
        Map<String, Object> entry = new LinkedHashMap<>();
        entry.put("openid", "o1");
        entry.put("KVDataList", Collections.singletonList(
                Collections.singletonMap("key", "score")));
        Map<String, Object> result = new LinkedHashMap<>();
        result.put("data", Collections.singletonList(entry));
        result.put("absent", null);
        HostDelegation.ecosystemSink(settlement).succeed(result);
        HostDelegation.ecosystemSink(settlement).fail(-1, "late");

        assertEquals(1, delivered.size());
        JSONObject answer = new JSONObject(delivered.get(0));
        assertEquals(5, answer.getInt("requestId"));
        assertEquals("score", answer.getJSONArray("data").getJSONObject(0)
                .getJSONArray("KVDataList").getJSONObject(0).getString("key"));
        assertFalse(answer.has("absent"));
        assertFalse(answer.has("error"));
    }

    private static String letters(int index) {
        return "" + (char) ('a' + index / 26) + (char) ('a' + index % 26);
    }
}
