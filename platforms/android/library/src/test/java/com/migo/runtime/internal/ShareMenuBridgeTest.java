package com.migo.runtime.internal;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import com.migo.runtime.callback.MenuShareCallback;
import com.migo.runtime.callback.MenuShareContent;
import com.migo.runtime.callback.ShareHandler;

import org.json.JSONObject;
import org.junit.After;
import org.junit.Test;

import java.util.ArrayList;
import java.util.List;

/**
 * The host's share menu through the Java SDK: a pick reaches the game, its
 * answer reaches the callback once, and the menu state reads as content set it.
 */
public final class ShareMenuBridgeTest {

    private static final int SESSION = 5151;

    @After
    public void forget() {
        ShareMenuBridge.forget(SESSION);
    }

    @Test
    public void a_pick_reaches_the_game_and_its_answer_reaches_the_callback_once() throws Exception {
        List<String> posted = new ArrayList<>();
        List<MenuShareContent> answers = new ArrayList<>();
        ShareMenuBridge.request(SESSION, MenuShareCallback.MENU_SHARE_APP_MESSAGE, answers::add,
                (session, json) -> posted.add(json));

        JSONObject event = new JSONObject(posted.get(0));
        assertEquals("shareAppMessage", event.getString("menu"));
        long replyId = event.getLong("replyId");

        String reply = "{\"replyId\":" + replyId + ",\"menu\":\"shareAppMessage\","
                + "\"content\":{\"title\":\"t\",\"imageUrl\":\"/data/x/card.png\",\"toCurrentGroup\":false}}";
        ShareMenuBridge.reply(SESSION, reply);
        ShareMenuBridge.reply(SESSION, reply);

        assertEquals(1, answers.size());
        MenuShareContent content = answers.get(0);
        assertEquals("t", content.title);
        assertEquals("/data/x/card.png", content.imageUrl);
        assertEquals("", content.query);
        assertFalse(content.toCurrentGroup);
        assertFalse(content.disableForward);
    }

    @Test
    public void no_answer_and_a_restart_both_reach_the_callback_as_null() throws Exception {
        List<Object> answers = new ArrayList<>();
        List<String> posted = new ArrayList<>();
        ShareMenuBridge.request(SESSION, MenuShareCallback.MENU_ADD_TO_FAVORITES, answers::add,
                (session, json) -> posted.add(json));
        ShareMenuBridge.request(SESSION, MenuShareCallback.MENU_SHARE_TIMELINE, answers::add,
                (session, json) -> posted.add(json));
        long first = new JSONObject(posted.get(0)).optLong("replyId");
        ShareMenuBridge.reply(SESSION, "{\"replyId\":" + first + ",\"content\":null}");
        ShareMenuBridge.abandon(SESSION);
        ShareMenuBridge.abandon(SESSION);

        assertEquals(2, answers.size());
        assertNull(answers.get(0));
        assertNull(answers.get(1));
    }

    @Test
    public void only_the_three_items_and_a_callback_are_taken() {
        assertThrows(IllegalArgumentException.class, () -> ShareMenuBridge.request(
                SESSION, "copyLink", content -> { }, (session, json) -> { }));
        assertThrows(IllegalArgumentException.class, () -> ShareMenuBridge.request(
                SESSION, MenuShareCallback.MENU_SHARE_APP_MESSAGE, null, (session, json) -> { }));
    }

    @Test
    public void the_menu_reads_as_the_game_set_it() throws Exception {
        ShareHandler.ShareMenu menu = HostDelegation.shareMenu(new JSONObject(
                "{\"menus\":[\"shareAppMessage\"],\"withShareTicket\":true,"
                        + "\"isUpdatableMessage\":true,\"activityId\":\"act\"}"));
        assertTrue(menu.shareAppMessage);
        assertFalse(menu.shareTimeline);
        assertTrue(menu.withShareTicket);
        assertEquals(Boolean.TRUE, menu.options.get("isUpdatableMessage"));
        assertEquals("act", menu.options.get("activityId"));
        assertEquals(2, menu.options.size());
    }
}
