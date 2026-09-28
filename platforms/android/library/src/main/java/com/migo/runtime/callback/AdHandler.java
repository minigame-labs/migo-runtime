package com.migo.runtime.callback;

/**
 * Host-provided advertising handler.
 * <p>
 * Register via {@link com.migo.runtime.GameSession#setAdHandler(AdHandler)}
 * before the game starts, then bridge each call to your ad SDK (Pangle,
 * GDT/Youliang Hui, Kuaishou Union, ...). The runtime links no ad SDK, holds no
 * ad credentials, and makes no decision about whether an advert was watched.
 *
 * <h2>Without a handler, or without this format</h2>
 * Content still gets working ad objects and no advert is displayed. Every call
 * settles, because a pending ad request is a stalled game: {@code createAd},
 * {@code loadAd} and {@code showAd} report an error, {@code showAd} also closes
 * with {@code isEnded = false}, and {@code hideAd} reports the ad as hidden.
 * <p>
 * The close carries {@code false} deliberately: a runtime that reported a
 * completed view with no advert behind it would have publishers paying out
 * rewards while earning nothing. It is reported at all because content following
 * the common mini-game platform's idiom decides its payout and resumes gameplay in {@code onClose} -- an
 * error alone leaves the player looking at a paused game.
 * <p>
 * A handler that sells only some formats gets the same treatment for the rest:
 * the defaults below are that settlement, so there is one behaviour to reason
 * about rather than one per way of not having an advert.
 *
 * <h2>Contract</h2>
 * <ul>
 *   <li>{@code adId} is a runtime-allocated handle. It identifies one ad object
 *       for its whole life; report every event for that ad with the same id.</li>
 *   <li>Calls arrive on the runtime's host thread. Do not block: hand work to
 *       your ad SDK and return. Events come back through the {@link AdEventSink},
 *       which is safe to use from any thread.</li>
 *   <li>Every request-bearing method has a default that settles as "not supported",
 *       so a handler need only implement the ad formats it actually sells.
 *       The layout-update and destroy methods are lifecycle operations and have
 *       no result to settle.</li>
 *   <li>After {@link #destroyAd}, emit nothing further for that {@code adId}.</li>
 * </ul>
 */
public interface AdHandler {

    /** Rewarded video: {@code adType} value for {@link #createAd}. */
    String TYPE_REWARDED_VIDEO = "rewardedVideo";
    /** Interstitial: {@code adType} value for {@link #createAd}. */
    String TYPE_INTERSTITIAL = "interstitial";
    /** Banner: {@code adType} value for {@link #createAd}. */
    String TYPE_BANNER = "banner";
    /** Custom (native-styled) ad: {@code adType} value for {@link #createAd}. */
    String TYPE_CUSTOM = "custom";
    /** Grid ad: {@code adType} value for {@link #createAd}. */
    String TYPE_GRID = "grid";
    /** Game recommendation banner: {@code adType} value for {@link #createAd}. */
    String TYPE_GAME_BANNER = "gameBanner";
    /** Game recommendation icon: {@code adType} value for {@link #createAd}. */
    String TYPE_GAME_ICON = "gameIcon";
    /** Game recommendation portal: {@code adType} value for {@link #createAd}. */
    String TYPE_GAME_PORTAL = "gamePortal";

    /**
     * Create an ad object.
     * <p>
     * Report readiness with {@link AdEventSink#emitLoad(int)}; report a failure
     * to create with {@link AdEventSink#emitError}.
     *
     * @param adId        runtime-allocated handle for this ad
     * @param adType      one of the {@code TYPE_*} constants
     * @param adUnitId    the publisher's ad slot id, as content supplied it
     * @param optionsJson remaining platform-style creation options as a JSON object
     *                    ({@code adIntervals}, {@code style}, {@code adTheme},
     *                    {@code gridCount}, {@code count}, {@code multiton});
     *                    never null, possibly {@code "{}"}
     * @param sink        channel to report this ad's events on
     */
    default void createAd(int adId, String adType, String adUnitId, String optionsJson,
                          AdEventSink sink) {
        sink.emitError(adId, -1, "createAd:fail not supported");
    }

    /**
     * Load (or reload) content for an existing ad.
     *
     * @param adId runtime-allocated handle
     * @param sink channel to report this ad's events on
     */
    default void loadAd(int adId, AdEventSink sink) {
        sink.emitError(adId, -1, "loadAd:fail not supported");
    }

    /**
     * Present the ad.
     * <p>
     * For rewarded video, when the advert is dismissed call
     * {@link AdEventSink#emitClose(int, boolean)} with your SDK's completion
     * verdict.
     *
     * @param adId runtime-allocated handle
     * @param sink channel to report this ad's events on
     */
    default void showAd(int adId, AdEventSink sink) {
        sink.emitShowFailed(adId, -1, "showAd:fail not supported");
    }

    /**
     * Hide a positioned ad without destroying it.
     *
     * @param adId runtime-allocated handle
     * @param sink channel to report this ad's events on
     */
    default void hideAd(int adId, AdEventSink sink) {
        // Hiding an ad format the host does not support is not an error --
        // content that calls hide() on an ad that never appeared has done
        // nothing wrong -- but it is not a silent drop either: a custom ad's
        // onHide is how content learns the space is free again.
        sink.emitHide(adId);
    }

    /**
     * Apply a layout change content made to a positioned ad's style.
     *
     * @param adId      runtime-allocated handle
     * @param styleJson the changed fields as a JSON object, in CSS pixels
     *                  (for example {@code {"top":120}})
     * @param sink      channel to report this ad's events on
     */
    default void updateAdStyle(int adId, String styleJson, AdEventSink sink) {
        // Ignoring a layout change is survivable; failing one is not worth
        // surfacing to content as an ad error.
    }

    /**
     * Release the ad. No further events may be emitted for {@code adId}.
     *
     * @param adId runtime-allocated handle
     */
    default void destroyAd(int adId) {
    }
}
