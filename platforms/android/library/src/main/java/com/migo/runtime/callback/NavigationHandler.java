package com.migo.runtime.callback;

import java.util.Map;

/**
 * Host-provided handler for leaving the game.
 * <p>
 * Register via
 * {@link com.migo.runtime.GameSession#setNavigationHandler(NavigationHandler)}
 * before the game starts.
 *
 * <h2>Why these two are one interface</h2>
 * Both take the user out of the game and into somewhere the host owns: another
 * mini program you decide whether to launch, or your own support channel. The
 * runtime cannot do either -- it has no app registry and no support desk -- and
 * a host that implements one almost always implements the other, because both
 * answer the same product question about where a player is allowed to go.
 *
 * <h2>Without a handler</h2>
 * {@code migo.navigateToMiniProgram()} fails with
 * {@code navigateToMiniProgram:fail not supported} and code {@code -2}, and
 * {@code migo.openCustomerServiceConversation()} fails with
 * {@code openCustomerServiceConversation:fail not supported}, and so does
 * {@code migo.navigateBackMiniProgram()} with its own name.
 *
 * <h2>Contract</h2>
 * <ul>
 *   <li>{@link #navigateToMiniProgram} must eventually settle, exactly once,
 *       through the {@link NavigationSink} it is given.</li>
 *   <li>{@link #openCustomerServiceConversation} and {@link #navigateBackMiniProgram}
 *       answer with their return value instead, because the runtime gave them no
 *       result channel -- see there.</li>
 *   <li>Calls arrive on the runtime's host thread. Do not block: start the
 *       Activity and return. The sink is safe to use from any thread.</li>
 * </ul>
 */
public interface NavigationHandler {

    /**
     * Take the user to another mini program.
     * <p>
     * Whether the destination may be launched at all is your decision: the
     * {@code appId} is whatever the game asked for, and a game that names an
     * app you do not host should be refused through
     * {@link NavigationSink#fail}.
     *
     * @param request where content asked to go
     * @param sink    channel to settle this request on
     */
    default void navigateToMiniProgram(NavigateRequest request, NavigationSink sink) {
        sink.fail(-2, "navigateToMiniProgram:fail not supported");
    }

    /**
     * Open your support channel.
     * <p>
     * Answered by the return value rather than a sink, and the asymmetry is the
     * engine's rather than a choice made here: this and
     * {@link #navigateBackMiniProgram} are the APIs in this group the runtime
     * exposes to content synchronously, with no result callback
     * defined for it on either side of the boundary. So there is nothing to
     * settle later -- {@code true} resolves the content-side call and
     * {@code false} rejects it with
     * {@code openCustomerServiceConversation:fail not supported}.
     * <p>
     * Return {@code true} once the conversation UI has been started, not once it
     * has been closed. Content is not told when the user leaves it.
     *
     * @param request the conversation content asked for
     * @return whether the conversation was opened
     */
    default boolean openCustomerServiceConversation(CustomerServiceRequest request) {
        return false;
    }

    /**
     * Return to the mini program that opened this one, handing it
     * {@code extraData} -- which it reads in its {@code onShow}.
     * <p>
     * Answered by the return value, as {@link #openCustomerServiceConversation}
     * is and for the same reason: content's call settles on it. {@code false}
     * -- this game was not opened by another, or you do not take players back --
     * rejects the call with {@code navigateBackMiniProgram:fail not supported}.
     *
     * @param extraData data for the mini program returned to, as an immutable
     *                  tree of {@code String}, {@code Number}, {@code Boolean},
     *                  {@code List} and {@code Map}; empty when content gave none
     * @return whether the player is being taken back
     */
    default boolean navigateBackMiniProgram(java.util.Map<String, Object> extraData) {
        return false;
    }

    /** Where content asked to go. */
    final class NavigateRequest {
        /** Target mini program's app id, as content supplied it; never empty. */
        public final String appId;
        /** Path within the target, empty when content named none. */
        public final String path;
        /**
         * Data to hand the target, as an immutable tree of {@code String},
         * {@code Boolean}, {@code Number}, {@code List} and {@code Map}. Empty
         * when content supplied none.
         */
        public final Map<String, Object> extraData;
        /**
         * Which build of the target content asked for: {@code "develop"},
         * {@code "trial"} or {@code "release"}. Defaults to {@code "release"}.
         */
        public final String envVersion;

        public NavigateRequest(String appId, String path, Map<String, Object> extraData,
                               String envVersion) {
            this.appId = appId;
            this.path = path;
            this.extraData = extraData;
            this.envVersion = envVersion;
        }
    }

    /** The conversation content asked for. */
    final class CustomerServiceRequest {
        /** Where in the game the user came from, empty when content set none. */
        public final String sessionFrom;
        /** Whether to prefill a card describing the game in the conversation. */
        public final boolean showMessageCard;
        /** Title for that card, empty when content set none. */
        public final String sendMessageTitle;
        /** In-game path that card links to, empty when content set none. */
        public final String sendMessagePath;
        /** Image for that card, empty when content set none. */
        public final String sendMessageImg;

        public CustomerServiceRequest(String sessionFrom, boolean showMessageCard,
                                      String sendMessageTitle, String sendMessagePath,
                                      String sendMessageImg) {
            this.sessionFrom = sessionFrom;
            this.showMessageCard = showMessageCard;
            this.sendMessageTitle = sendMessageTitle;
            this.sendMessagePath = sendMessagePath;
            this.sendMessageImg = sendMessageImg;
        }
    }
}
