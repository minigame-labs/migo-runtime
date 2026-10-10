package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;

import com.migo.runtime.internal.CallbackCorrelation;

import org.json.JSONArray;
import org.json.JSONObject;
import org.junit.Test;

import java.util.Arrays;
import java.util.Collections;
import java.util.List;

/**
 * Every image result answers the request that asked for it.
 *
 * <p>{@code chooseImage}, {@code chooseMessageFile} and {@code compressImage} are
 * deferred APIs: the runtime allocates an id, sends it in the request JSON, and
 * matches the reply by it. A reply that drops the id falls back to settling the
 * <em>oldest</em> pending request, so two pickers open at once settle each
 * other's promises.
 *
 * <p>These tests build the results the manager sends without touching Android,
 * because the correlation is a property of the JSON, not of the picker.
 */
public final class ImageApiResultCorrelationTest {

    @Test
    public void a_chosen_image_result_carries_the_id_of_the_request_that_chose_it() throws Exception {
        JSONObject result = new JSONObject(ImageApiManager.chooseImageResultJson(
                4242,
                Arrays.asList("/tmp/a.jpg", "/tmp/b.jpg"),
                Arrays.asList(11L, 22L)));

        assertEquals(4242, result.getInt("requestId"));

        JSONArray paths = result.getJSONArray("tempFilePaths");
        assertEquals(2, paths.length());
        assertEquals("/tmp/a.jpg", paths.getString(0));
        assertEquals("/tmp/b.jpg", paths.getString(1));

        JSONArray files = result.getJSONArray("tempFiles");
        assertEquals(2, files.length());
        assertEquals("/tmp/b.jpg", files.getJSONObject(1).getString("path"));
        assertEquals(22L, files.getJSONObject(1).getLong("size"));
    }

    @Test
    public void a_chosen_image_result_for_an_id_less_request_claims_no_id() throws Exception {
        JSONObject result = new JSONObject(ImageApiManager.chooseImageResultJson(
                CallbackCorrelation.ABSENT,
                Collections.singletonList("/tmp/a.jpg"),
                Collections.singletonList(11L)));

        // Not `requestId: 0`: the runtime discards a reply whose id is present
        // and invalid, where an omitted key still settles through the fallback.
        assertFalse(result.has("requestId"));
        assertEquals(1, result.getJSONArray("tempFilePaths").length());
    }

    @Test
    public void a_chosen_message_file_result_carries_its_id_and_the_file_records() throws Exception {
        List<JSONObject> files = Arrays.asList(
                new JSONObject().put("path", "/tmp/one.pdf").put("size", 5L),
                new JSONObject().put("path", "/tmp/two.pdf").put("size", 6L));

        JSONObject result =
                new JSONObject(ImageApiManager.chooseMessageFileResultJson(31337, files));

        assertEquals(31337, result.getInt("requestId"));
        assertEquals(2, result.getJSONArray("tempFiles").length());
        assertEquals("/tmp/two.pdf",
                result.getJSONArray("tempFiles").getJSONObject(1).getString("path"));
    }

    @Test
    public void a_compressed_image_result_carries_its_id_beside_the_temp_path() throws Exception {
        JSONObject result =
                new JSONObject(ImageApiManager.compressImageResultJson(9, "/tmp/compress_1.jpg"));

        assertEquals(9, result.getInt("requestId"));
        assertEquals("/tmp/compress_1.jpg", result.getString("tempFilePath"));
    }

    @Test
    public void two_results_built_for_two_requests_stay_distinguishable() throws Exception {
        // The property the FIFO fallback cannot provide: with two pickers in
        // flight, each reply names its own request rather than whichever one
        // started first.
        JSONObject first = new JSONObject(ImageApiManager.chooseImageResultJson(
                1001, Collections.singletonList("/tmp/first.jpg"), Collections.singletonList(1L)));
        JSONObject second = new JSONObject(ImageApiManager.chooseImageResultJson(
                1002, Collections.singletonList("/tmp/second.jpg"), Collections.singletonList(2L)));

        assertEquals(1001, first.getInt("requestId"));
        assertEquals(1002, second.getInt("requestId"));
        assertEquals("/tmp/first.jpg", first.getJSONArray("tempFilePaths").getString(0));
        assertEquals("/tmp/second.jpg", second.getJSONArray("tempFilePaths").getString(0));
    }

    @Test
    public void a_chosen_media_result_names_its_kind_from_its_items() throws Exception {
        JSONObject photo = new JSONObject().put("tempFilePath", "/c/a.jpg").put("fileType", "image");
        JSONObject clip = new JSONObject().put("tempFilePath", "/c/b.mp4").put("fileType", "video")
                .put("thumbTempFilePath", "/c/b.jpg");

        JSONObject images = new JSONObject(ImageApiManager.chooseMediaResultJson(
                3, Collections.singletonList(photo)));
        JSONObject videos = new JSONObject(ImageApiManager.chooseMediaResultJson(
                4, Collections.singletonList(clip)));
        JSONObject mixed = new JSONObject(ImageApiManager.chooseMediaResultJson(
                5, Arrays.asList(photo, clip)));

        assertEquals(3, images.getInt("requestId"));
        assertEquals("image", images.getString("type"));
        assertEquals("video", videos.getString("type"));
        assertEquals("mix", mixed.getString("type"));
        assertEquals("/c/b.jpg",
                mixed.getJSONArray("tempFiles").getJSONObject(1).getString("thumbTempFilePath"));
    }

    @Test
    public void a_request_that_answers_only_success_still_names_its_request() throws Exception {
        assertEquals(12, new JSONObject(ImageApiManager.doneJson(12)).getInt("requestId"));
        assertFalse(new JSONObject(ImageApiManager.doneJson(CallbackCorrelation.ABSENT))
                .has("requestId"));
    }

    @Test
    public void a_cancelled_picker_reports_failure_to_the_request_it_cancelled() throws Exception {
        JSONObject failure =
                new JSONObject(CallbackCorrelation.failure(77, "chooseImage", "cancel"));

        assertEquals(77, failure.getInt("requestId"));
        assertEquals("chooseImage:fail cancel", failure.getString("error"));
    }

    @Test
    public void a_failure_reason_carrying_a_quote_stays_one_json_document() throws Exception {
        // The hand-rolled `msg.replace("\"", "\\\"")` this replaced escaped
        // quotes but not the backslash before them, so a reason like this one
        // produced JSON the runtime could not parse -- and an unparseable reply
        // settles nothing at all.
        JSONObject failure = new JSONObject(CallbackCorrelation.failure(
                5, "compressImage", "path C:\\pics\\\"a\".jpg is unreadable"));

        assertEquals(5, failure.getInt("requestId"));
        assertEquals("compressImage:fail path C:\\pics\\\"a\".jpg is unreadable",
                failure.getString("error"));
    }
}
