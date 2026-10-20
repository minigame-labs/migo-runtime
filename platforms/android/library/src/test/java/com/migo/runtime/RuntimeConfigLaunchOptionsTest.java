package com.migo.runtime;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;

import org.json.JSONObject;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.RuntimeEnvironment;
import org.robolectric.annotation.Config;

import java.lang.reflect.Field;

@RunWith(RobolectricTestRunner.class)
@Config(sdk = 35)
public final class RuntimeConfigLaunchOptionsTest {
    /** The field the engine reads by name when it creates the session (jni/inbound.rs, "launchOptionsJson"). */
    private static String launchOptionsJson(RuntimeConfig config) throws Exception {
        Field field = RuntimeConfig.class.getDeclaredField("launchOptionsJson");
        field.setAccessible(true);
        return (String) field.get(config);
    }

    /**
     * What the host launches the game with reaches the engine as the object it set, as it was when set; none is
     * null, which the engine launches with its defaults.
     */
    @Test
    public void theLaunchOptionsReachTheEngineAsTheObjectTheHostSet() throws Exception {
        JSONObject options = new JSONObject("{\"scene\":1007,\"query\":{\"level\":\"2\"}}");
        RuntimeConfig config =
                new RuntimeConfig.Builder(RuntimeEnvironment.getApplication()).setLaunchOptions(options).build();
        options.put("scene", 1038);

        assertEquals(1007, new JSONObject(launchOptionsJson(config)).getInt("scene"));
        assertEquals("2", new JSONObject(launchOptionsJson(config)).getJSONObject("query").getString("level"));
        assertNull(launchOptionsJson(new RuntimeConfig.Builder(RuntimeEnvironment.getApplication()).build()));
        assertNull(launchOptionsJson(new RuntimeConfig.Builder(RuntimeEnvironment.getApplication())
                .setLaunchOptions(options).setLaunchOptions(null).build()));
    }
}
