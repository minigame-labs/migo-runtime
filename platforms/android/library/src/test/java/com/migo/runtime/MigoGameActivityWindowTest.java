package com.migo.runtime;

import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import android.content.Intent;
import android.view.Window;

import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.Robolectric;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.android.controller.ActivityController;
import org.robolectric.annotation.Config;

@RunWith(RobolectricTestRunner.class)
@Config(sdk = 35)
public final class MigoGameActivityWindowTest {
    /**
     * A host's app theme with an action bar does not put one over its games.
     *
     * The game activity takes the app's theme unless its own manifest entry names one, and
     * neither the SDK's nor the game-center kit's does: an app on a framework theme with an
     * action bar ran its game under the app's title bar, in a surface shorter than the screen.
     * Launched without a game here, so the launch fails before any native code runs; the window
     * is already decided by then.
     */
    @Test
    public void aGameHasNoTitleBarUnderAnAppThemeThatHasOne() {
        ActivityController<MigoGameActivity> controller =
                Robolectric.buildActivity(MigoGameActivity.class, new Intent());
        controller.get().setTheme(android.R.style.Theme_Material_Light);
        controller.create();

        MigoGameActivity activity = controller.get();
        assertTrue("the window has no title",
                activity.getWindow().hasFeature(Window.FEATURE_NO_TITLE));
        assertNull("and no action bar", activity.getActionBar());
    }
}
