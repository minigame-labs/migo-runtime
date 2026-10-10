package com.migo.runtime.callback;

import java.util.Map;

/**
 * Host-provided handler for your app's own ecosystem: friends and groups, cloud
 * storage, live channels, voice chat, handoff, subscriptions, privacy
 * agreements -- the features a host app offers on top of the game.
 * <p>
 * Register via
 * {@link com.migo.runtime.GameSession#setEcosystemHandler(EcosystemHandler)}
 * before the game starts.
 *
 * <h2>One method, keyed by name</h2>
 * What each of these APIs means is yours, not the runtime's, so they arrive as
 * one request carrying the content API's name ({@code getGroupCloudStorage},
 * {@code requestSubscribeMessage}, ...) -- an object's member as
 * {@code Class.member} ({@code GameServerManager.createRoom}) -- and its
 * options, rather than as an interface per API. The names that can arrive are a closed list -- the
 * {@code ecosystem} service in the runtime's host-service contract -- so a
 * handler can switch over them and fail the rest:
 * <pre>{@code
 * switch (request.api) {
 *     case "getGroupEnterInfo": ...; break;
 *     default: sink.fail(-2, request.api + ":fail not supported");
 * }
 * }</pre>
 * Events go the other way, through
 * {@link com.migo.runtime.GameSession#postEcosystemEvent}, and the synchronous
 * getters ({@code getExtConfigSync}, {@code isChatTool}, ...) answer with what
 * {@link com.migo.runtime.GameSession#setEcosystemValue} last reported.
 *
 * <h2>Without a handler</h2>
 * There is no ecosystem. The few APIs with a true answer for that give it --
 * {@code getPrivacySetting} needs no authorization, {@code getExtConfig} is
 * empty, {@code checkIsAddedToMyMiniProgram} is not added -- and every other one
 * fails with {@code <api>:fail not supported} and code {@code -2}.
 *
 * <h2>Contract</h2>
 * <ul>
 *   <li>Every request must eventually settle, exactly once, through the
 *       {@link EcosystemSink} it is given. Many wait on the player -- a group
 *       picker, a live room -- so the runtime sets no deadline.</li>
 *   <li>A path among the options ({@code shareImageToGroup}'s
 *       {@code imagePath}, ...) is a real file path, resolved inside the game's
 *       sandbox, readable until the request settles.</li>
 *   <li>Calls arrive on the runtime's host thread. Do not block: start the work
 *       and return. The sink is safe to use from any thread.</li>
 * </ul>
 */
public interface EcosystemHandler {

    /**
     * Answer one request.
     *
     * @param request the API content called and the options it passed
     * @param sink    channel to settle this request on
     */
    void call(Request request, EcosystemSink sink);

    /** One ecosystem API call. */
    final class Request {
        /**
         * The content API's name, or an object member's as {@code Class.member};
         * one of the contract's, never empty.
         */
        public final String api;
        /**
         * The options content passed, without its callbacks, as an immutable tree
         * of {@code Map}, {@code List}, {@code String}, {@code Number},
         * {@code Boolean} and {@code null}; empty when it passed none.
         */
        public final Map<String, Object> options;

        public Request(String api, Map<String, Object> options) {
            this.api = api;
            this.options = options;
        }
    }
}
