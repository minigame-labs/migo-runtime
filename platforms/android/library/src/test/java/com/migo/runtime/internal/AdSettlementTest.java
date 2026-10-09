package com.migo.runtime.internal;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import com.migo.runtime.callback.AdEventSink;
import com.migo.runtime.callback.AdHandler;

import java.util.ArrayList;
import java.util.EnumMap;
import java.util.Arrays;
import java.util.List;
import java.util.Map;

import org.junit.Test;

/**
 * What content is owed when no advert can be shown.
 *
 * <p>The runtime installs an ad service on every full-profile Android session,
 * so {@code migo.createRewardedVideoAd()} takes the hosted path whether or not the
 * embedder ever registered an {@link AdHandler}. That makes "hosted, but there is
 * no handler" the ordinary state of an integration in progress, and it used to be
 * the state in which {@code hide()}, {@code updateStyle()} and {@code destroy()}
 * dropped the request without a word, while {@code show()} reported an error and
 * no close -- leaving a rewarded-video flow that follows the common mini-game platform's idiom
 * ({@code onClose} decides the payout) waiting forever.
 *
 * <p>{@code NativeExports} itself cannot be loaded here: it holds
 * {@code android.os.Handler} statics and this module deliberately has no
 * Robolectric. {@code AdOp} can, because it depends on the sink interface alone,
 * which is also what keeps the settlement decision out of the untestable half.
 */
public final class AdSettlementTest {

    /** Records what content would see, in order. */
    private static final class RecordingSink implements AdEventSink {
        final List<String> events = new ArrayList<>();

        @Override
        public void emitLoad(int adId) {
            events.add("load:" + adId);
        }

        @Override
        public void emitLoad(int adId, boolean useFallbackSharePage) {
            events.add("load:" + adId + ":" + useFallbackSharePage);
        }

        @Override
        public void emitShow(int adId) {
            events.add("show:" + adId);
        }

        @Override
        public void emitError(int adId, int errCode, String errMsg) {
            events.add("error:" + adId + ":" + errCode + ":" + errMsg);
        }

        @Override
        public void emitClose(int adId, boolean isEnded) {
            events.add("close:" + adId + ":" + isEnded);
        }

        @Override
        public void emitResize(int adId, int width, int height) {
            events.add("resize:" + adId);
        }

        @Override
        public void emitHide(int adId) {
            events.add("hide:" + adId);
        }
    }

    private static final int AD_ID = 7;

    /**
     * The settlement each command owes, spelled out rather than derived, so that
     * a change of behaviour has to be a change of expectation.
     */
    private static Map<NativeExports.AdOp, List<String>> expectations() {
        Map<NativeExports.AdOp, List<String>> expected =
                new EnumMap<>(NativeExports.AdOp.class);
        expected.put(NativeExports.AdOp.CREATE,
                Arrays.asList("error:7:-1:createAd:fail no ad handler"));
        expected.put(NativeExports.AdOp.LOAD,
                Arrays.asList("error:7:-1:loadAd:fail no ad handler"));
        // Both halves: the error says why, the close lets content continue. The
        // verdict is false, so nothing is minted.
        expected.put(NativeExports.AdOp.SHOW,
                Arrays.asList("error:7:-1:showAd:fail no ad handler", "close:7:false"));
        expected.put(NativeExports.AdOp.HIDE, Arrays.asList("hide:7"));
        // Nothing is owed for these two, and that is a decision, not an omission:
        // the common mini-game platform has no callback for a style write, and release is terminal.
        expected.put(NativeExports.AdOp.UPDATE_STYLE, Arrays.<String>asList());
        expected.put(NativeExports.AdOp.DESTROY, Arrays.<String>asList());
        return expected;
    }

    @Test
    public void everyAdCommandSettlesWhatContentIsWaitingFor() {
        Map<NativeExports.AdOp, List<String>> expected = expectations();

        // A seventh ad command must fail here until its settlement is stated.
        // Iterating values() rather than a hardcoded three is the whole point:
        // the defect being fixed was three commands nobody had thought about.
        assertEquals("every ad command needs a stated settlement",
                NativeExports.AdOp.values().length, expected.size());

        for (NativeExports.AdOp op : NativeExports.AdOp.values()) {
            RecordingSink sink = new RecordingSink();
            op.settleWithoutAdvert(sink, AD_ID, "no ad handler");
            assertEquals(op.name(), expected.get(op), sink.events);
        }
    }

    @Test
    public void aRewardedVideoThatCannotBeShownStillCloses() {
        // The case a publisher hits first: the SDK is integrated, no ad SDK is
        // wired up yet, and content waits in onClose. Named separately from the
        // table above because this one stalls a game rather than merely losing
        // an event -- and because a table is data somebody can edit to make a
        // failure go away, while this claim cannot be satisfied that way.
        RecordingSink sink = new RecordingSink();
        NativeExports.AdOp.SHOW.settleWithoutAdvert(sink, AD_ID, "no ad handler");

        assertTrue("show must report a close: " + sink.events,
                sink.events.contains("close:" + AD_ID + ":false"));
    }

    @Test
    public void theSettlementNeverMintsAReward() {
        // Deliberately blind to whether a close is reported at all, so that this
        // and the test above fail for different reasons: dropping the close
        // leaves this one green, and only a truthy verdict turns it red.
        for (NativeExports.AdOp op : NativeExports.AdOp.values()) {
            RecordingSink sink = new RecordingSink();
            op.settleWithoutAdvert(sink, AD_ID, "no ad handler");

            assertTrue(op.name() + " reported a completed view with no advert: "
                            + sink.events,
                    !sink.events.contains("close:" + AD_ID + ":true"));
        }
    }

    @Test
    public void theInterfaceDefaultsSettleExactlyAsTheMissingHandlerDoes() {
        // Two ways to have no advert -- no handler at all, and a handler that
        // does not sell this format -- and one behaviour, or content has to cope
        // with both. The defaults are the second path, so they are checked
        // against the same table.
        AdHandler defaults = new AdHandler() {};

        for (NativeExports.AdOp op : NativeExports.AdOp.values()) {
            RecordingSink viaDefaults = new RecordingSink();
            invokeDefault(defaults, op, viaDefaults);

            RecordingSink viaSettlement = new RecordingSink();
            op.settleWithoutAdvert(viaSettlement, AD_ID, "not supported");

            assertEquals(op.name(), viaSettlement.events, viaDefaults.events);
        }
    }

    /** Call the {@link AdHandler} method the command forwards to. */
    private static void invokeDefault(AdHandler handler, NativeExports.AdOp op,
                                      AdEventSink sink) {
        switch (op) {
            case CREATE:
                handler.createAd(AD_ID, AdHandler.TYPE_REWARDED_VIDEO, "unit", "{}", sink);
                break;
            case LOAD:
                handler.loadAd(AD_ID, sink);
                break;
            case SHOW:
                handler.showAd(AD_ID, sink);
                break;
            case HIDE:
                handler.hideAd(AD_ID, sink);
                break;
            case UPDATE_STYLE:
                handler.updateAdStyle(AD_ID, "{}", sink);
                break;
            case DESTROY:
                handler.destroyAd(AD_ID);
                break;
            default:
                throw new AssertionError("no AdHandler call mapped for " + op);
        }
    }
}
