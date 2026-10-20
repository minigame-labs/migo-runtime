package com.migo.runtime;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertSame;
import static org.junit.Assert.assertTrue;

import android.content.Intent;

import com.migo.runtime.callback.GameSessionListener;

import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.Robolectric;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.annotation.Config;

@RunWith(RobolectricTestRunner.class)
@Config(sdk = 35)
public final class MigoGameActivityExitTest {
    /**
     * A game that exits (exitMiniProgram) closes its activity, as it closes in any host. With no
     * listener of the subclass's nothing finished it, and the activity stayed open on a session that
     * had stopped. Not created, so no launch runs and finishes it first.
     */
    @Test
    public void aGameThatExitsFinishesTheActivity() {
        MigoGameActivity activity =
                Robolectric.buildActivity(MigoGameActivity.class, new Intent()).get();
        GameSessionListener listener = activity.sessionListener();
        assertFalse("running", activity.isFinishing());

        listener.onGameExit(0);

        assertTrue("the game's exit finishes the activity", activity.isFinishing());
    }

    /** A subclass that takes the session's events takes the exit too: its listener is the one used. */
    @Test
    public void aSubclassListenerIsTheSessionsListener() {
        GameSessionListener own = new GameSessionListener() {
            @Override public void onGameReady() {}
            @Override public void onGameExit(int exitCode) {}
            @Override public void onError(MigoException exception) {}
        };
        OwnListener activity = Robolectric.buildActivity(OwnListener.class, new Intent()).get();
        activity.own = own;

        assertSame(own, activity.sessionListener());
    }

    public static final class OwnListener extends MigoGameActivity {
        GameSessionListener own;

        @Override
        protected GameSessionListener onCreateGameListener() {
            return own;
        }
    }
}
