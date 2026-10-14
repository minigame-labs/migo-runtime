package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import android.app.Activity;
import android.provider.Settings;
import android.view.WindowManager;

import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.Robolectric;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.annotation.Config;

/** What getScreenBrightness answers: the window's own brightness, or the system's where the window follows it. */
@RunWith(RobolectricTestRunner.class)
@Config(sdk = 35)
public final class ScreenBrightnessTest {

    private static Activity activity() {
        return Robolectric.buildActivity(Activity.class).setup().get();
    }

    /**
     * A window that never set a brightness follows the system's: its override reads -1, which is not a brightness,
     * and was answered as "no activity". The answer is the system's setting, 0..255 scaled to 0..1.
     */
    @Test
    public void a_window_that_follows_the_system_reports_the_system_setting() {
        Activity activity = activity();
        assertEquals(WindowManager.LayoutParams.BRIGHTNESS_OVERRIDE_NONE,
                activity.getWindow().getAttributes().screenBrightness, 0f);
        Settings.System.putInt(activity.getContentResolver(), Settings.System.SCREEN_BRIGHTNESS, 51);
        assertEquals(0.2f, ScreenBrightness.getBrightness(activity), 1e-6f);
    }

    @Test
    public void a_window_s_own_brightness_is_reported_as_set() {
        Activity activity = activity();
        assertEquals(0, ScreenBrightness.setBrightness(activity, 0.75f));
        assertEquals(0.75f, ScreenBrightness.getBrightness(activity), 1e-6f);
    }

    @Test
    public void no_window_to_ask_is_not_a_brightness() {
        assertTrue(Float.isNaN(ScreenBrightness.getBrightness(null)));
    }
}
