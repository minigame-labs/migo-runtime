package com.migo.runtime.internal;

import com.migo.runtime.callback.MenuShareCallback;
import com.migo.runtime.callback.MenuShareContent;

import org.json.JSONException;
import org.json.JSONObject;

/**
 * The share items of the host's menu, per session: the requests posted to the
 * game and the callbacks waiting for its answers.
 *
 * <p>Apart from {@code NativeExports}, which holds {@code android.*} statics, so
 * it is reachable from a host-JVM unit test.
 */
final class ShareMenuBridge {

    private ShareMenuBridge() {}

    /** Where a request goes: {@code NativeMethods.onShareMenuEvent}. */
    interface EventChannel {
        void deliver(int sessionId, String eventJson);
    }

    private static final ReplyRegistry<MenuShareCallback> sReplies = new ReplyRegistry<>();

    static boolean isMenu(String menu) {
        return MenuShareCallback.MENU_SHARE_APP_MESSAGE.equals(menu)
                || MenuShareCallback.MENU_SHARE_TIMELINE.equals(menu)
                || MenuShareCallback.MENU_ADD_TO_FAVORITES.equals(menu);
    }

    /**
     * Ask the game what to share for {@code menu}.
     *
     * @throws IllegalArgumentException for a menu that is not one of
     *         {@link MenuShareCallback}'s, or a null callback
     */
    static void request(
            int sessionId, String menu, MenuShareCallback callback, EventChannel channel) {
        if (!isMenu(menu)) throw new IllegalArgumentException("not a share menu item: " + menu);
        if (callback == null) throw new IllegalArgumentException("no callback");
        long replyId = sReplies.newId();
        String json;
        try {
            json = new JSONObject().put("menu", menu).put("replyId", replyId).toString();
        } catch (JSONException impossible) {
            throw new IllegalStateException(impossible);
        }
        sReplies.put(sessionId, replyId, callback);
        channel.deliver(sessionId, json);
    }

    /** {@code {"replyId", "menu", "content"}} from the game: hand it to its callback. */
    static void reply(int sessionId, String replyJson) {
        JSONObject answer = HostDelegation.options(replyJson);
        Object raw = answer.opt("replyId");
        if (!(raw instanceof Integer || raw instanceof Long)) return;
        MenuShareCallback waiting = sReplies.take(sessionId, ((Number) raw).longValue(), true);
        if (waiting == null) return;
        JSONObject content = answer.optJSONObject("content");
        waiting.onShareContent(content != null ? content(content) : null);
    }

    /**
     * Answer every request the session still waits on with null: the runtime that
     * would have answered is gone. Each is answered even if one throws; the first
     * exception is rethrown after.
     */
    static void abandon(int sessionId) {
        RuntimeException first = null;
        for (MenuShareCallback waiting : sReplies.abandon(sessionId)) {
            try {
                waiting.onShareContent(null);
            } catch (RuntimeException thrown) {
                if (first == null) first = thrown;
            }
        }
        if (first != null) throw first;
    }

    static void forget(int sessionId) {
        sReplies.forget(sessionId);
    }

    static MenuShareContent content(JSONObject content) {
        return new MenuShareContent(
                content.optString("title", ""),
                content.optString("imageUrl", ""),
                content.optString("query", ""),
                content.optString("imageUrlId", ""),
                content.optBoolean("toCurrentGroup", true),
                content.optString("path", ""),
                content.optString("imagePreviewUrl", ""),
                content.optString("imagePreviewUrlId", ""),
                content.optBoolean("disableForward", false));
    }
}
