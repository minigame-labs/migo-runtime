package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import android.content.Context;

import com.migo.runtime.RuntimeConfig;

import org.json.JSONObject;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.RuntimeEnvironment;
import org.robolectric.annotation.Config;

/** What the app tells the engine about itself, for getAppBaseInfo and its kin. */
@RunWith(RobolectricTestRunner.class)
@Config(sdk = 35)
public final class HostAppInfoTest {

    private static RuntimeConfig.Builder builder(Context context) {
        return new RuntimeConfig.Builder(context);
    }

    @Test
    @Config(qualifiers = "fr-rCA")
    public void the_app_names_itself_its_language_and_text_scale() throws Exception {
        Context context = RuntimeEnvironment.getApplication();
        context.getResources().getConfiguration().fontScale = 1.15f;
        JSONObject json = new JSONObject(HostAppInfo.toJson(context, builder(context).build(), true));
        assertEquals(context.getPackageName(), json.getString("hostAppId"));
        assertEquals("fr-CA", json.getString("language"));
        // The float's shortest decimal, not its binary expansion.
        assertEquals(1.15, json.getDouble("fontScale"), 0.0);
        assertTrue(json.getBoolean("debug"));
    }

    @Test
    public void an_unconfigured_app_reports_a_release_with_no_version_on_an_unknown_device()
            throws Exception {
        Context context = RuntimeEnvironment.getApplication();
        JSONObject json = new JSONObject(HostAppInfo.toJson(context, builder(context).build(), false));
        assertEquals(-1, json.getInt("benchmarkLevel"));
        assertEquals(0, json.getInt("modelLevel"));
        assertEquals("release", json.getString("contentEnvironment"));
        assertFalse(json.has("contentVersion"));
        assertFalse(json.getBoolean("debug"));
    }

    @Test
    public void what_the_app_configured_is_what_it_reports() throws Exception {
        Context context = RuntimeEnvironment.getApplication();
        RuntimeConfig config = builder(context)
                .setContentEnvironment(RuntimeConfig.ContentEnvironment.TRIAL)
                .setContentVersion("1.4.2")
                .setDeviceBenchmark(31, 1)
                .build();
        JSONObject json = new JSONObject(HostAppInfo.toJson(context, config, false));
        assertEquals("trial", json.getString("contentEnvironment"));
        assertEquals("1.4.2", json.getString("contentVersion"));
        assertEquals(31, json.getInt("benchmarkLevel"));
        assertEquals(1, json.getInt("modelLevel"));
    }

    @Test
    public void the_builder_refuses_what_the_engine_could_not_report() {
        Context context = RuntimeEnvironment.getApplication();
        assertThrows(IllegalArgumentException.class, () -> builder(context).setDeviceBenchmark(-3, 0));
        assertThrows(IllegalArgumentException.class, () -> builder(context).setDeviceBenchmark(10, 4));
        assertThrows(IllegalArgumentException.class, () -> builder(context).setContentEnvironment(null));
        StringBuilder longVersion = new StringBuilder();
        for (int i = 0; i <= RuntimeConfig.CONTENT_VERSION_MAX_BYTES; i++) longVersion.append('9');
        assertThrows(IllegalArgumentException.class,
                () -> builder(context).setContentVersion(longVersion.toString()));
        // Empty is none.
        assertEquals(null, builder(context).setContentVersion("").build().getContentVersion());
    }
}
