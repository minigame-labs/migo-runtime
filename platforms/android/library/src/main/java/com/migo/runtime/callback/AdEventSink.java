package com.migo.runtime.callback;

/**
 * The channel an {@link AdHandler} reports ad activity on.
 * <p>
 * The runtime hands one of these to every {@link AdHandler} call. It is the
 * only way ad events reach content: the runtime itself never synthesises a
 * load, a close, or -- most importantly -- a reward.
 *
 * <h2>Reward integrity</h2>
 * {@link #emitClose(int, boolean)} carries the verdict that decides whether the
 * player gets paid. Pass through what your ad SDK reports and nothing else:
 * <pre>{@code
 * rewardedAd.setListener(new RewardAdListener() {
 *     public void onRewardVerify(boolean verified, ...) {
 *         sink.emitClose(adId, verified);   // right: the SDK decided
 *     }
 *     public void onAdClose() {
 *         sink.emitClose(adId, true);       // WRONG: pays for a skipped ad
 *     }
 * });
 * }</pre>
 * A close without a completed view is still a close -- report it with
 * {@code isEnded = false} rather than not reporting it, or content will wait
 * forever for an event that never comes.
 *
 * <h2>Threading</h2>
 * Every method is safe to call from any thread, which matters because ad SDKs
 * deliver their callbacks on their own threads. Events are queued to the
 * runtime and delivered to content in order.
 *
 * <p>Emitting for an ad that has already been destroyed is harmless and does
 * nothing; you do not need to race teardown to stay correct.
 */
public interface AdEventSink {

    /**
     * Report that ad content is ready to show.
     *
     * @param adId the handle passed to {@link AdHandler#createAd}
     */
    void emitLoad(int adId);

    /**
     * Report that ad content is ready to show, for a rewarded video that will
     * fall back to a share page instead of a video.
     *
     * @param adId                  the handle passed to {@link AdHandler#createAd}
     * @param useFallbackSharePage  whether content should expect a share page
     */
    void emitLoad(int adId, boolean useFallbackSharePage);

    /**
     * Report that the ad is on screen -- the SDK's exposure callback.
     * <p>
     * This is what settles content's {@code show()} Promise: an advert that
     * was asked for is not shown until the SDK says so. A full-screen ad that
     * closes is taken as shown too, but a banner, grid, custom or game banner
     * ad has no close, so without this its {@code show()} never settles.
     *
     * @param adId the handle passed to {@link AdHandler#createAd}
     */
    void emitShow(int adId);

    /**
     * Report a failure. Content receives this on its {@code onError} listener.
     *
     * @param adId    the handle passed to {@link AdHandler#createAd}
     * @param errCode SDK-specific error code, or -1 if there is none
     * @param errMsg  human-readable reason; must not be null
     */
    void emitError(int adId, int errCode, String errMsg);

    /**
     * Report that a full-screen ad was dismissed.
     * <p>
     * For rewarded video, {@code isEnded} must be your ad SDK's own completion
     * verdict -- see the class javadoc. For interstitial and portal ads the
     * flag is ignored.
     *
     * @param adId    the handle passed to {@link AdHandler#createAd}
     * @param isEnded whether the SDK confirmed the advert was watched to the end
     */
    void emitClose(int adId, boolean isEnded);

    /**
     * Report that a full-screen ad could not be shown at all.
     * <p>
     * Both halves matter. The error tells content (and your logcat) why, and the
     * close is what lets content continue: the common mini-game platform's idiom decides the payout and
     * resumes gameplay in {@code onClose}, so a rewarded video that reports only
     * an error leaves the player looking at a paused game forever. The verdict is
     * {@code false}, so nothing is minted -- this reports the absence of an
     * advert, it does not invent one.
     * <p>
     * Ad formats with no {@code close} event ignore the second half, so this is
     * safe to use for any {@code showAd} failure.
     *
     * @param adId    the handle passed to {@link AdHandler#createAd}
     * @param errCode SDK-specific error code, or -1 if there is none
     * @param errMsg  human-readable reason; must not be null
     */
    default void emitShowFailed(int adId, int errCode, String errMsg) {
        emitError(adId, errCode, errMsg);
        emitClose(adId, false);
    }

    /**
     * Report the rendered size of a positioned ad, in CSS pixels.
     * <p>
     * These are CSS pixels, not physical pixels: divide by the display density
     * you were given at attach time. Reporting physical pixels makes content
     * lay out around a box several times too large.
     *
     * @param adId   the handle passed to {@link AdHandler#createAd}
     * @param width  rendered width in CSS pixels
     * @param height rendered height in CSS pixels
     */
    void emitResize(int adId, int width, int height);

    /**
     * Report that a custom ad was hidden by the user or the SDK.
     *
     * @param adId the handle passed to {@link AdHandler#createAd}
     */
    void emitHide(int adId);
}
