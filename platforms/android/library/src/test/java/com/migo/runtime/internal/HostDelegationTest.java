package com.migo.runtime.internal;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import com.migo.runtime.callback.NavigationHandler;
import com.migo.runtime.callback.NavigationSink;
import com.migo.runtime.callback.PaymentHandler;
import com.migo.runtime.callback.PaymentSink;
import com.migo.runtime.callback.SettingHandler;
import com.migo.runtime.callback.SettingSink;
import com.migo.runtime.callback.ShareHandler;
import com.migo.runtime.callback.ShareSink;

import org.json.JSONObject;
import org.junit.Test;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * What content is owed when the host owns the answer and has not supplied one.
 *
 * <p>Settings, share, navigation and payment are the embedder's, not the runtime's, and
 * every one of them used to be a hardcoded failure inside {@code NativeExports}. Replacing
 * those with a delegation seam is only safe if registering no handler changes nothing
 * content can observe -- an embedder who has not integrated payment must see exactly the
 * reply they saw before -- so the pre-existing strings and codes are asserted literally
 * here rather than derived from the code that produces them.
 *
 * <p>{@code NativeExports} itself cannot be loaded in this module: it holds
 * {@code android.os.Handler} statics and there is deliberately no Robolectric.
 * {@code HostDelegation} can, which is why the parse and the settlement live there.
 */
public final class HostDelegationTest {

    private static final int SESSION = 3;
    private static final int REQUEST_ID = 91;

    /** Records the result JSON the runtime would route to content, in order. */
    private static final class RecordingChannel implements HostDelegation.ResultChannel {
        final List<String> results = new ArrayList<>();

        @Override
        public void deliver(int sessionId, String resultJson) {
            assertEquals(SESSION, sessionId);
            results.add(resultJson);
        }

        String only() {
            assertEquals("expected exactly one result: " + results, 1, results.size());
            return results.get(0);
        }
    }

    private static HostDelegation.Settlement settlement(RecordingChannel channel) {
        return settlement(channel, REQUEST_ID);
    }

    private static HostDelegation.Settlement settlement(RecordingChannel channel, int requestId) {
        return new HostDelegation.Settlement(SESSION, requestId, channel, () -> false);
    }

    // ---- the no-handler defaults ---------------------------------------------------

    /**
     * The exact reply each unimplemented method produced before there was a handler to
     * delegate to, spelled out rather than derived.
     *
     * <p>These strings reach content as {@code errMsg}, and content branches on them: a
     * game that checks for {@code "not supported"} to hide its store keeps working only
     * while the text is unchanged. The code {@code -2} is likewise the value content reads
     * as {@code errCode}.
     */
    @Test
    public void everyDefaultSettlesExactlyAsTheHardcodedFailureDid() throws Exception {
        Map<String, Runnable> defaults = new LinkedHashMap<>();
        Map<String, RecordingChannel> channels = new LinkedHashMap<>();

        for (String api : Arrays.asList(
                "openSetting",
                "shareAppMessage",
                "navigateToMiniProgram",
                "requestMidasPayment",
                "requestMidasPaymentGameItem")) {
            channels.put(api, new RecordingChannel());
        }

        SettingHandler settingDefaults = new SettingHandler() {};
        ShareHandler shareDefaults = new ShareHandler() {};
        NavigationHandler navigationDefaults = new NavigationHandler() {};
        PaymentHandler paymentDefaults = new PaymentHandler() {};

        defaults.put("openSetting", () -> settingDefaults.openSetting(
                HostDelegation.settingSink(settlement(channels.get("openSetting")))));
        defaults.put("shareAppMessage", () -> shareDefaults.shareAppMessage(
                HostDelegation.shareRequest(new JSONObject()),
                HostDelegation.shareSink(settlement(channels.get("shareAppMessage")))));
        defaults.put("navigateToMiniProgram", () -> navigationDefaults.navigateToMiniProgram(
                HostDelegation.navigateRequest(new JSONObject()),
                HostDelegation.navigationSink(
                        settlement(channels.get("navigateToMiniProgram")))));
        defaults.put("requestMidasPayment", () -> paymentDefaults.requestMidasPayment(
                HostDelegation.paymentRequest(new JSONObject()),
                HostDelegation.paymentSink(settlement(channels.get("requestMidasPayment")))));
        defaults.put("requestMidasPaymentGameItem",
                () -> paymentDefaults.requestMidasPaymentGameItem(
                        HostDelegation.gameItemPaymentRequest(new JSONObject()),
                        HostDelegation.paymentSink(
                                settlement(channels.get("requestMidasPaymentGameItem")))));

        for (Map.Entry<String, Runnable> each : defaults.entrySet()) {
            String api = each.getKey();
            each.getValue().run();

            JSONObject result = new JSONObject(channels.get(api).only());
            assertEquals(api, api + ":fail not supported", result.getString("error"));
            assertEquals(api, -2, result.getInt("errCode"));
            assertEquals(api, REQUEST_ID, result.getInt("requestId"));
        }
    }

    /**
     * The one default that is not a settlement: {@code checkIsSupportMidasPayment} answered
     * {@code allow_pay: false} synchronously and must keep doing so, because content reads
     * it to decide whether to open a store at all.
     */
    @Test
    public void paymentSupportDefaultsToNoPaymentChannel() {
        assertFalse(new PaymentHandler() {}.isMidasPaymentSupported());
    }

    /**
     * Customer service answers by return value, not through a sink, so its "not supported"
     * is a {@code false} rather than a result document. Asserted separately for that
     * reason: it is the one API in this group with no result channel on either side of the
     * JNI boundary.
     */
    @Test
    public void customerServiceDefaultsToNotOpened() {
        assertFalse(new NavigationHandler() {}.openCustomerServiceConversation(
                HostDelegation.customerServiceRequest(new JSONObject())));
    }

    /**
     * A request that carried no id must get a reply with no {@code requestId} key at all.
     * Stamping {@code 0} makes the runtime discard the reply as "present and not an id",
     * which is worse than the FIFO fallback an omitted key still takes.
     */
    @Test
    public void aRequestWithoutAnIdIsAnsweredWithoutOne() throws Exception {
        RecordingChannel channel = new RecordingChannel();
        settlement(channel, CallbackCorrelation.ABSENT).fail(-2, "openSetting:fail not supported");

        JSONObject result = new JSONObject(channel.only());
        assertFalse(result.has("requestId"));
        assertEquals("openSetting:fail not supported", result.getString("error"));
    }

    // ---- a registered handler receives the parsed values ---------------------------

    @Test
    public void aShareHandlerReceivesTheParsedShareFields() {
        ShareHandler.ShareRequest request = HostDelegation.shareRequest(HostDelegation.options(
                "{\"requestId\":91,\"title\":\"Beat my score\",\"imageUrl\":\"https://x/i.png\","
                        + "\"query\":\"lvl=7&ref=a\",\"imageUrlId\":\"image-2\"}"));

        assertEquals("Beat my score", request.title);
        assertEquals("https://x/i.png", request.imageUrl);
        assertEquals("lvl=7&ref=a", request.query);
        assertEquals("image-2", request.imageUrlId);
        assertTrue("a share goes back to its group unless content says not", request.toCurrentGroup);
        assertEquals("", request.path);
    }

    @Test
    public void aFriendShareCarriesItsQueryAndAnAbsentSceneIsNull() {
        ShareHandler.FriendShareRequest request =
                HostDelegation.friendShareRequest(HostDelegation.options(
                        "{\"requestId\":4,\"openId\":\"friend\",\"imageUrl\":\"/data/x/card.png\","
                                + "\"query\":\"room=7\",\"shareMessageToFriendScene\":3}"));
        assertEquals("friend", request.openId);
        assertEquals("/data/x/card.png", request.imageUrl);
        assertEquals("", request.title);
        assertEquals("room=7", request.query);
        assertEquals(Integer.valueOf(3), request.shareMessageToFriendScene);

        assertNull(HostDelegation.friendShareRequest(HostDelegation.options(
                "{\"openId\":\"friend\"}")).shareMessageToFriendScene);
    }

    @Test
    public void anImageShareCarriesItsEntrance() {
        ShareHandler.ImageShareRequest request =
                HostDelegation.imageShareRequest(HostDelegation.options(
                        "{\"requestId\":5,\"path\":\"/data/x/card.png\",\"needShowEntrance\":true}"));
        assertEquals("/data/x/card.png", request.path);
        assertTrue(request.needShowEntrance);
        assertEquals("", request.entrancePath);
    }

    @Test
    public void aNavigationHandlerReceivesTheParsedDestination() {
        NavigationHandler.NavigateRequest request =
                HostDelegation.navigateRequest(HostDelegation.options(
                        "{\"requestId\":91,\"appId\":\"other-app\",\"path\":\"pages/x\","
                                + "\"extraData\":{\"from\":\"lobby\",\"score\":42,"
                                + "\"tags\":[\"a\",\"b\"],\"nested\":{\"k\":true}},"
                                + "\"envVersion\":\"trial\"}"));

        assertEquals("other-app", request.appId);
        assertEquals("pages/x", request.path);
        assertEquals("trial", request.envVersion);
        assertEquals("lobby", request.extraData.get("from"));
        assertEquals(42, request.extraData.get("score"));
        assertEquals(Arrays.asList("a", "b"), request.extraData.get("tags"));

        // No org.json type may cross the published boundary: on Android that class is the
        // platform's, not the one this module compiles against, so a handler taking one
        // would be taking a type the SDK does not own.
        @SuppressWarnings("unchecked")
        Map<String, Object> nested = (Map<String, Object>) request.extraData.get("nested");
        assertEquals(Boolean.TRUE, nested.get("k"));
    }

    /** What content sent is not the host's to edit. */
    @Test(expected = UnsupportedOperationException.class)
    public void navigationExtraDataIsImmutable() {
        HostDelegation.navigateRequest(
                        HostDelegation.options("{\"extraData\":{\"from\":\"lobby\"}}"))
                .extraData.put("from", "elsewhere");
    }

    @Test
    public void aNavigationHandlerReceivesTheParsedConversationRequest() {
        NavigationHandler.CustomerServiceRequest request =
                HostDelegation.customerServiceRequest(HostDelegation.options(
                        "{\"sessionFrom\":\"shop\",\"showMessageCard\":true,"
                                + "\"sendMessageTitle\":\"Refund\",\"sendMessagePath\":\"p/1\","
                                + "\"sendMessageImg\":\"https://x/c.png\"}"));

        assertEquals("shop", request.sessionFrom);
        assertTrue(request.showMessageCard);
        assertEquals("Refund", request.sendMessageTitle);
        assertEquals("p/1", request.sendMessagePath);
        assertEquals("https://x/c.png", request.sendMessageImg);
    }

    @Test
    public void aPaymentHandlerReceivesTheParsedOrder() {
        PaymentHandler.PaymentRequest request =
                HostDelegation.paymentRequest(HostDelegation.options(
                        "{\"requestId\":91,\"mode\":\"game\",\"env\":1,\"offerId\":\"1450000\","
                                + "\"currencyType\":\"USD\",\"platform\":\"android\","
                                + "\"buyQuantity\":60,\"zoneId\":9,\"outTradeNo\":\"T-1\"}"));

        assertEquals("game", request.mode);
        assertEquals(1, request.env);
        assertEquals("1450000", request.offerId);
        assertEquals("USD", request.currencyType);
        assertEquals("android", request.platform);
        assertEquals(60, request.buyQuantity);
        assertEquals(9, request.zoneId);
        assertEquals("T-1", request.outTradeNo);
    }

    /**
     * The platform defaults for the fields content left out, which are not all empty: a request
     * parsed as {@code zoneId = 0} or {@code currencyType = ""} is a different order from
     * the one the platform would have sent.
     */
    @Test
    public void anOrderWithNothingSetCarriesThePlatformDefaults() {
        PaymentHandler.PaymentRequest request =
                HostDelegation.paymentRequest(new JSONObject());

        assertEquals("game", request.mode);
        assertEquals(0, request.env);
        assertEquals("CNY", request.currencyType);
        assertEquals(1, request.zoneId);
    }

    /** Likewise for navigation, whose omitted {@code envVersion} means "release". */
    @Test
    public void aDestinationWithNoEnvVersionMeansRelease() {
        assertEquals("release", HostDelegation.navigateRequest(new JSONObject()).envVersion);
        assertTrue(HostDelegation.navigateRequest(new JSONObject()).extraData.isEmpty());
    }

    @Test
    public void aGameItemOrderCarriesItsSignaturesVerbatim() {
        PaymentHandler.GameItemPaymentRequest request =
                HostDelegation.gameItemPaymentRequest(HostDelegation.options(
                        "{\"signData\":\"{\\\"offerId\\\":\\\"1\\\"}\",\"paySig\":\"ps\","
                                + "\"signature\":\"sg\"}"));

        assertEquals("{\"offerId\":\"1\"}", request.signData);
        assertEquals("ps", request.paySig);
        assertEquals("sg", request.signature);
    }

    /**
     * A malformed request still owes content an answer.
     *
     * <p>The engine builds this JSON, so a malformed one is a runtime bug rather than an
     * embedder's -- but content is awaiting a reply either way, and dropping the request
     * stalls it. Parsing as empty and settling is what keeps the failure loud in logcat and
     * survivable in the game.
     */
    @Test
    public void aMalformedRequestStillSettles() throws Exception {
        RecordingChannel channel = new RecordingChannel();
        HostDelegation.Settlement settlement = new HostDelegation.Settlement(
                SESSION,
                CallbackCorrelation.requestIdOf(HostDelegation.options("not json at all")),
                channel,
                () -> false);
        new ShareHandler() {}.shareAppMessage(
                HostDelegation.shareRequest(HostDelegation.options("not json at all")),
                HostDelegation.shareSink(settlement));

        JSONObject result = new JSONObject(channel.only());
        assertFalse(result.has("requestId"));
        assertEquals("shareAppMessage:fail not supported", result.getString("error"));
    }

    // ---- a request settles exactly once -------------------------------------------

    /**
     * The second settlement is rejected, in every ordering.
     *
     * <p>Two results for one {@code requestId} both reach content: for a purchase that
     * means {@code success} and {@code fail} both firing, and content that credits an item
     * in {@code success} crediting it twice. Hosts hit this by accident, because a payment
     * SDK calling both its completion and its dismissal callback is ordinary -- so it is
     * enforced rather than documented.
     */
    @Test
    public void onlyTheFirstSettlementReachesContent() throws Exception {
        RecordingChannel afterSuccess = new RecordingChannel();
        PaymentSink paid = HostDelegation.paymentSink(settlement(afterSuccess));
        paid.settlePaid();
        paid.fail(-1, "requestMidasPayment:fail user cancelled");
        paid.settlePaid();
        assertFalse("a completion must not be followed by a failure",
                new JSONObject(afterSuccess.only()).has("error"));

        RecordingChannel afterFailure = new RecordingChannel();
        PaymentSink cancelled = HostDelegation.paymentSink(settlement(afterFailure));
        cancelled.fail(-1, "requestMidasPayment:fail user cancelled");
        cancelled.settlePaid();
        assertEquals("requestMidasPayment:fail user cancelled",
                new JSONObject(afterFailure.only()).getString("error"));
    }

    /** The same guarantee on the other three domains, since each has its own sink. */
    @Test
    public void everyDomainSettlesAtMostOnce() {
        RecordingChannel setting = new RecordingChannel();
        SettingSink settingSink = HostDelegation.settingSink(settlement(setting));
        settingSink.settleOpened(null);
        settingSink.fail(-2, "openSetting:fail not supported");
        assertEquals(1, setting.results.size());

        RecordingChannel share = new RecordingChannel();
        ShareSink shareSink = HostDelegation.shareSink(settlement(share));
        shareSink.settleShared();
        shareSink.settleShared();
        assertEquals(1, share.results.size());

        RecordingChannel navigation = new RecordingChannel();
        NavigationSink navigationSink =
                HostDelegation.navigationSink(settlement(navigation));
        navigationSink.fail(-1, "navigateToMiniProgram:fail refused");
        navigationSink.settleNavigated();
        assertEquals(1, navigation.results.size());
    }

    /**
     * A settled request reads as settled, so a host that asks can tell.
     *
     * <p>Also what keeps the claim above from passing vacuously: a {@code compareAndSet}
     * that always returned true would still deliver one result if the channel dropped the
     * rest, and this observes the claim itself.
     */
    @Test
    public void settlementIsObservable() {
        RecordingChannel channel = new RecordingChannel();
        HostDelegation.Settlement settlement = settlement(channel);

        assertFalse(settlement.isSettled());
        settlement.fail(-2, "openSetting:fail not supported");
        assertTrue(settlement.isSettled());
    }

    /** A session that has ended is not owed a reply, and must not be sent one. */
    @Test
    public void aTerminatedSessionIsSentNothing() {
        RecordingChannel channel = new RecordingChannel();
        new HostDelegation.Settlement(SESSION, REQUEST_ID, channel, () -> true)
                .fail(-2, "openSetting:fail not supported");

        assertTrue(channel.results.isEmpty());
    }

    // ---- the success paths --------------------------------------------------------

    /**
     * {@code openSetting} is the only one of these with a success payload the engine
     * consumes: {@code createDeferredApi} copies every non-{@code requestId} key onto what
     * content receives, and {@code migo.openSetting()} is documented to answer
     * {@code res.authSetting}. The other three carry no fields, so their success is the
     * absence of an error.
     */
    @Test
    public void anOpenedSettingScreenReportsTheScopesItShowed() throws Exception {
        RecordingChannel channel = new RecordingChannel();
        Map<String, Boolean> scopes = new LinkedHashMap<>();
        scopes.put("scope.camera", true);
        scopes.put("scope.userLocation", false);
        HostDelegation.settingSink(settlement(channel)).settleOpened(scopes);

        JSONObject result = new JSONObject(channel.only());
        assertEquals(REQUEST_ID, result.getInt("requestId"));
        assertFalse(result.has("error"));
        JSONObject authSetting = result.getJSONObject("authSetting");
        assertTrue(authSetting.getBoolean("scope.camera"));
        assertFalse(authSetting.getBoolean("scope.userLocation"));
    }

    /**
     * A settings screen that grants nothing still succeeds, and still carries the
     * field: content reads {@code res.authSetting[scope]} off a success without
     * checking, so an absent object is a TypeError rather than a refusal.
     */
    @Test
    public void aSettingScreenThatReportsNoScopesStillSucceedsWithAnEmptyObject() throws Exception {
        RecordingChannel channel = new RecordingChannel();
        HostDelegation.settingSink(settlement(channel)).settleOpened(null);

        JSONObject result = new JSONObject(channel.only());
        assertEquals(REQUEST_ID, result.getInt("requestId"));
        assertFalse(result.has("error"));
        assertTrue(result.has("authSetting"));
        assertEquals(0, result.getJSONObject("authSetting").length());
    }

    @Test
    public void aSuccessCarriesNoErrorKeyOnAnyDomain() throws Exception {
        RecordingChannel share = new RecordingChannel();
        HostDelegation.shareSink(settlement(share)).settleShared();
        assertFalse(new JSONObject(share.only()).has("error"));

        RecordingChannel navigation = new RecordingChannel();
        HostDelegation.navigationSink(settlement(navigation)).settleNavigated();
        assertFalse(new JSONObject(navigation.only()).has("error"));

        RecordingChannel payment = new RecordingChannel();
        HostDelegation.paymentSink(settlement(payment)).settlePaid();
        assertFalse(new JSONObject(payment.only()).has("error"));
    }

    /** A reason with quotes in it must stay one JSON document, not two broken ones. */
    @Test
    public void aFailureReasonIsEscapedRatherThanConcatenated() throws Exception {
        RecordingChannel channel = new RecordingChannel();
        HostDelegation.paymentSink(settlement(channel))
                .fail(4, "requestMidasPayment:fail provider said \"declined\" \\ retry");

        JSONObject result = new JSONObject(channel.only());
        assertEquals("requestMidasPayment:fail provider said \"declined\" \\ retry",
                result.getString("error"));
        assertEquals(4, result.getInt("errCode"));
    }

    /** A host that reports no reason still produces a readable failure. */
    @Test
    public void aNullReasonStillProducesAReadableFailure() throws Exception {
        RecordingChannel channel = new RecordingChannel();
        HostDelegation.paymentSink(settlement(channel)).fail(-1, null);

        assertEquals("unknown error", new JSONObject(channel.only()).getString("error"));
    }

    /** A JSON null in content's payload arrives as a Java null, not as a sentinel object. */
    @Test
    public void aJsonNullInExtraDataArrivesAsNull() {
        Map<String, Object> extraData = HostDelegation.navigateRequest(
                HostDelegation.options("{\"extraData\":{\"k\":null}}")).extraData;

        assertTrue(extraData.containsKey("k"));
        assertNull(extraData.get("k"));
    }
}
