package com.migo.runtime.callback;

/**
 * Host-provided share handler.
 * <p>
 * Register via
 * {@link com.migo.runtime.GameSession#setShareHandler(ShareHandler)} before the
 * game starts, then bridge each call to your own share surface -- a system
 * chooser, your app's friend picker, a social SDK. The runtime links no share
 * SDK and holds no social graph.
 *
 * <h2>Without a handler</h2>
 * {@code migo.shareAppMessage()}, {@code migo.shareMessageToFriend()} and
 * {@code migo.showShareImageMenu()} fail with {@code <api>:fail not supported}
 * and code {@code -2}. They settle rather than stay silent: content commonly
 * awaits a share before resuming, so a dropped request is a paused game.
 *
 * <h2>Contract</h2>
 * <ul>
 *   <li>Each method must eventually settle, exactly once, through the
 *       {@link ShareSink} it is given. A share waits on the player for as long
 *       as they take; the runtime sets no deadline.</li>
 *   <li>Calls arrive on the runtime's host thread. Do not block: present your
 *       share surface and return. The sink is safe to use from any thread.</li>
 *   <li>A request is what content passed to the call, as it passed it. An image
 *       path in it is a real file, resolved inside the game's sandbox and
 *       readable until the request settles.</li>
 * </ul>
 */
public interface ShareHandler {

    /**
     * Present the share flow for one {@code migo.shareAppMessage()} call.
     *
     * @param request what content asked to share
     * @param sink    channel to settle this request on
     */
    default void shareAppMessage(ShareRequest request, ShareSink sink) {
        sink.fail(-2, "shareAppMessage:fail not supported");
    }

    /**
     * Share to one friend of the player's, for one
     * {@code migo.shareMessageToFriend()} call.
     *
     * @param request the friend and what content asked to share with them
     * @param sink    channel to settle this request on
     */
    default void shareMessageToFriend(FriendShareRequest request, ShareSink sink) {
        sink.fail(-2, "shareMessageToFriend:fail not supported");
    }

    /**
     * Present your share sheet for one image -- send it to a friend, keep it,
     * save it -- for one {@code migo.showShareImageMenu()} call.
     *
     * @param request the image and how its message opens the game
     * @param sink    channel to settle this request on
     */
    default void showShareImageMenu(ImageShareRequest request, ShareSink sink) {
        sink.fail(-2, "showShareImageMenu:fail not supported");
    }

    /**
     * Your menu's share items, as the game set them -- after every
     * {@code showShareMenu}, {@code hideShareMenu} and {@code updateShareMenu}.
     * A game's share items start hidden. When the player picks one, ask the game
     * what to share with {@link com.migo.runtime.GameSession#requestMenuShare}.
     *
     * @param menu the menu's whole state now
     */
    default void onShareMenuChanged(ShareMenu menu) {
    }

    /** What content asked to share. */
    final class ShareRequest {
        /** Share title, empty when content set none. */
        public final String title;
        /**
         * Share image: an http(s) URL, or a real file path resolved inside the
         * game's sandbox; empty when content set none.
         */
        public final String imageUrl;
        /**
         * Query string the launched game should receive, empty when content set
         * none. The common mini-game platform spells it {@code a=1&b=2}, without a leading {@code ?}.
         */
        public final String query;
        /**
         * platform image id, empty when content set none. Meaningful only to
         * a host that resolves ids against that platform; ignore it otherwise.
         */
        public final String imageUrlId;
        /**
         * Whether a share made from a group's toolbar goes back to that group;
         * true unless content said otherwise.
         */
        public final boolean toCurrentGroup;
        /** The independent subpackage the share opens, empty when content named none. */
        public final String path;

        public ShareRequest(
                String title,
                String imageUrl,
                String query,
                String imageUrlId,
                boolean toCurrentGroup,
                String path) {
            this.title = title;
            this.imageUrl = imageUrl;
            this.query = query;
            this.imageUrlId = imageUrlId;
            this.toCurrentGroup = toCurrentGroup;
            this.path = path;
        }
    }

    /** One friend, and what content asked to share with them. */
    final class FriendShareRequest {
        /** The friend's open id, as content supplied it; never empty. */
        public final String openId;
        /** Share title, empty when content set none. */
        public final String title;
        /** A real file path resolved inside the game's sandbox, empty when content set none. */
        public final String imageUrl;
        /** Platform image id, empty when content set none. */
        public final String imageUrlId;
        /**
         * What the friend's enter options carry as {@code query.query}, from
         * {@code migo.setMessageToFriendQuery()}; empty when content set none.
         */
        public final String query;
        /**
         * What they carry as {@code query.shareMessageToFriendScene}, 0 to 50; null
         * when content set none.
         */
        public final Integer shareMessageToFriendScene;

        public FriendShareRequest(
                String openId,
                String title,
                String imageUrl,
                String imageUrlId,
                String query,
                Integer shareMessageToFriendScene) {
            this.openId = openId;
            this.title = title;
            this.imageUrl = imageUrl;
            this.imageUrlId = imageUrlId;
            this.query = query;
            this.shareMessageToFriendScene = shareMessageToFriendScene;
        }
    }

    /** The share items of your menu. */
    final class ShareMenu {
        /** Whether "share" is offered. */
        public final boolean shareAppMessage;
        /** Whether "share to moments" is offered; never without "share". */
        public final boolean shareTimeline;
        /** Whether a share carries a share ticket. */
        public final boolean withShareTicket;
        /**
         * The updatable-message fields the game set through {@code updateShareMenu}
         * ({@code isUpdatableMessage}, {@code activityId}, {@code templateInfo},
         * ...), as an immutable tree; empty when it set none.
         */
        public final java.util.Map<String, Object> options;

        public ShareMenu(
                boolean shareAppMessage,
                boolean shareTimeline,
                boolean withShareTicket,
                java.util.Map<String, Object> options) {
            this.shareAppMessage = shareAppMessage;
            this.shareTimeline = shareTimeline;
            this.withShareTicket = withShareTicket;
            this.options = options;
        }
    }

    /** One image for your share sheet. */
    final class ImageShareRequest {
        /** A real file path resolved inside the game's sandbox; never empty. */
        public final String path;
        /** Whether the image's message carries an entry into the game. */
        public final boolean needShowEntrance;
        /** Where that entry opens the game, empty for its default. */
        public final String entrancePath;

        public ImageShareRequest(String path, boolean needShowEntrance, String entrancePath) {
            this.path = path;
            this.needShowEntrance = needShowEntrance;
            this.entrancePath = entrancePath;
        }
    }
}
