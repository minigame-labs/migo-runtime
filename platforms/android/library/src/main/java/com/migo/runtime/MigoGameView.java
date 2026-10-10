package com.migo.runtime;

import android.app.Activity;
import android.app.Application;
import android.content.ComponentCallbacks2;
import android.content.Context;
import android.content.res.Configuration;
import android.graphics.Rect;
import android.os.Bundle;
import android.util.Log;
import android.view.MotionEvent;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.ViewGroup;
import android.widget.FrameLayout;

import com.migo.runtime.ErrorCode;
import com.migo.runtime.MigoException;
import com.migo.runtime.callback.GameSessionListener;
import com.migo.runtime.internal.NativeMethods;
import com.migo.runtime.internal.ThreadCheck;
import com.migo.runtime.internal.platform.DisplayCompat;
import com.migo.runtime.internal.platform.OrientationWaitHelper;
import com.migo.runtime.internal.platform.GameSurfaceView;

/**
 * Self-contained game view that manages the full game lifecycle internally.
 * <p>
 * This is the recommended way to embed a game in your layout. It handles:
 * <ul>
 *   <li>Surface creation, change, and destruction</li>
 *   <li>Touch event dispatch</li>
 *   <li>Activity lifecycle (pause/resume/destroy) via ActivityLifecycleCallbacks</li>
 *   <li>Memory warning forwarding</li>
 *   <li>Debug overlay (when enabled)</li>
 * </ul>
 *
 * <h3>Usage</h3>
 * <pre>{@code
 * MigoGameView gameView = new MigoGameView(context);
 * RuntimeConfig config = new RuntimeConfig.Builder(context)
 *     .setImmersiveMode(false)  // usually false for embedded views
 *     .build();
 * gameView.setConfig(config);
 * gameView.setGameListener(listener);
 * layout.addView(gameView);
 * gameView.loadGame("my-game", "game.js");
 * }</pre>
 *
 */
public class MigoGameView extends FrameLayout implements SurfaceHolder.Callback {

    private static final String TAG = "MigoGameView";

    private SurfaceView surfaceView;
    private volatile GameSession session;
    private RuntimeConfig config;
    private GameSessionListener gameListener;
    private SessionCreatedListener sessionCreatedListener;
    private String pendingGameId;
    private String pendingEntryPoint;
    private boolean surfaceReady = false;
    private final OrientationWaitHelper orientationHelper = new OrientationWaitHelper(TAG);
    private String appliedStartupOrientation;
    private String lastOrientationEventValue;
    private Activity boundActivity;
    private Application.ActivityLifecycleCallbacks lifecycleCallbacks;
    private ComponentCallbacks2 memoryCallback;

    public MigoGameView(Context context) {
        super(context);
        init();
    }

    private void init() {
        surfaceView = new GameSurfaceView(getContext());
        addView(surfaceView, new LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT));
        surfaceView.getHolder().addCallback(this);
    }

    /**
     * Set the runtime configuration.
     * Must be called before {@link #loadGame}.
     *
     * @param config Runtime configuration
     */
    public void setConfig(RuntimeConfig config) {
        this.config = config;
    }

    /**
     * Set the game event listener.
     *
     * @param listener Listener for game events, or null to remove
     */
    public void setGameListener(GameSessionListener listener) {
        this.gameListener = listener;
        if (session != null) {
            session.setListener(listener);
        }
    }

    /**
     * Set a callback that fires when GameSession is created.
     *
     * This callback runs before startGame, so host handlers can be registered
     * without polling.
     */
    public void setSessionCreatedListener(SessionCreatedListener listener) {
        this.sessionCreatedListener = listener;
        if (listener != null && session != null && session.isValid()) {
            listener.onSessionCreated(session);
        }
    }

    /**
     * Load and start a game.
     * <p>
     * If the surface is not ready yet, the game will start automatically
     * when the surface becomes available.
     *
     * @param gameId     Unique game identifier
     * @param entryPoint Entry point file (e.g., "game.js")
     */
    public void loadGame(String gameId, String entryPoint) {
        this.pendingGameId = gameId;
        this.pendingEntryPoint = entryPoint;
        orientationHelper.reset();
        this.appliedStartupOrientation = null;
        this.lastOrientationEventValue = null;

        if (surfaceReady) {
            SurfaceHolder holder = surfaceView.getHolder();
            Rect frame = holder.getSurfaceFrame();
            tryStartSessionOrDefer(holder, frame.width(), frame.height());
        }
    }

    /**
     * Get the current game session, if active.
     *
     * @return The active GameSession, or null
     */
    public GameSession getSession() {
        return session;
    }

    // ==================== SurfaceHolder.Callback ====================

    @Override
    public void surfaceCreated(SurfaceHolder holder) {
        surfaceReady = true;
        if (session != null && session.isValid()) {
            orientationHelper.cancel();
            Rect frame = holder.getSurfaceFrame();
            session.updateSurface(holder.getSurface(), frame.width(), frame.height());
        } else if (pendingGameId != null) {
            Rect frame = holder.getSurfaceFrame();
            tryStartSessionOrDefer(holder, frame.width(), frame.height());
        }
    }

    @Override
    public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {
        if (session != null && session.isValid()) {
            orientationHelper.cancel();
            session.updateSurface(holder.getSurface(), width, height);
        } else if (pendingGameId != null) {
            tryStartSessionOrDefer(holder, width, height);
        }
    }

    @Override
    public void surfaceDestroyed(SurfaceHolder holder) {
        surfaceReady = false;
        orientationHelper.cancel();
        if (session != null && session.isValid()) {
            session.onSurfaceDestroyed();
        }
        // Do NOT close the session. The surface is recreated when the view
        // becomes visible again. Session cleanup happens in onDetachedFromWindow.
    }

    // ==================== Touch ====================

    @Override
    public boolean onTouchEvent(MotionEvent event) {
        if (session != null && session.isValid()) {
            return session.dispatchTouchEvent(event);
        }
        return super.onTouchEvent(event);
    }

    // ==================== Lifecycle ====================

    @Override
    protected void onAttachedToWindow() {
        ThreadCheck.ensureMainThread();
        super.onAttachedToWindow();
        boundActivity = findActivity(getContext());
        if (boundActivity != null) {
            registerLifecycleCallbacks(boundActivity);
        }
    }

    @Override
    protected void onDetachedFromWindow() {
        ThreadCheck.ensureMainThread();
        unregisterLifecycleCallbacks();
        orientationHelper.cancel();
        destroySession();
        super.onDetachedFromWindow();
    }

    // ==================== Private ====================

    private void tryStartSessionOrDefer(SurfaceHolder holder, int width, int height) {
        if (session != null || pendingGameId == null) {
            return;
        }

        RuntimeConfig cfg = ensureConfig();
        Activity activity = resolveActivity();
        applyStartupOrientationIfNeeded(cfg, activity);

        if (orientationHelper.getTargetOrientation() != null
                && !orientationHelper.surfaceMatches(width, height)) {
            orientationHelper.defer(holder, h -> {
                if (session != null || pendingGameId == null) return;
                startSession(h, ensureConfig(), resolveActivity());
            });
            return;
        }

        orientationHelper.cancel();
        startSession(holder, cfg, activity);
    }

    private void startSession(SurfaceHolder holder, RuntimeConfig cfg, Activity activity) {
        if (session != null || pendingGameId == null) {
            return;
        }

        MigoRuntime runtime = MigoRuntime.getInstance();
        if (!runtime.isNativeLoaded()) {
            return;
        }

        if (activity == null) {
            notifySessionError(
                    ErrorCode.ERR_INVALID_ACTIVITY,
                    ErrorCode.getMessage(ErrorCode.ERR_INVALID_ACTIVITY),
                    false
            );
            return;
        }

        MigoRuntime.Result<GameSession> result = runtime.createSessionSafe(
                activity,
                holder.getSurface(),
                cfg,
                pendingGameId
        );

        if (result.isFailure()) {
            notifySessionError(
                    result.getErrorCode(),
                    "createSession failed: " + result.getErrorMessage(),
                    false
            );
            return;
        }

        session = result.getValue();
        // The pointer's icon and capture on a desktop-form device are this view's.
        session.setInputView(surfaceView);

        if (sessionCreatedListener != null) {
            try {
                sessionCreatedListener.onSessionCreated(session);
            } catch (Throwable t) {
                Log.e(TAG, "sessionCreatedListener error", t);
            }
        }

        if (activity != null) {
            lastOrientationEventValue = DisplayCompat.mapDeviceOrientationValue(activity,
                    activity.getResources().getConfiguration());
        } else {
            lastOrientationEventValue = null;
        }

        if (gameListener != null) {
            session.setListener(gameListener);
        }

        int startResult = session.startGameSafe(pendingEntryPoint);
        if (startResult != ErrorCode.SUCCESS) {
            notifySessionError(startResult, ErrorCode.getMessage(startResult), false);
            destroySession();
        }
    }

    private void destroySession() {
        if (session != null) {
            session.close();
            session = null;
        }
        orientationHelper.cancel();
        lastOrientationEventValue = null;
    }

    private void registerLifecycleCallbacks(Activity activity) {
        lifecycleCallbacks = new EmptyActivityLifecycleCallbacks() {
            @Override
            public void onActivityPaused(Activity a) {
                if (a == activity && session != null && session.isValid()) {
                    session.pause();
                }
            }

            @Override
            public void onActivityResumed(Activity a) {
                if (a == activity && session != null && session.isValid()) {
                    session.resume();
                }
            }

            @Override
            public void onActivityDestroyed(Activity a) {
                if (a == activity) {
                    destroySession();
                }
            }
        };
        activity.getApplication().registerActivityLifecycleCallbacks(lifecycleCallbacks);

        memoryCallback = new ComponentCallbacks2() {
            @Override
            public void onTrimMemory(int level) {
                if (session != null && session.isValid()) {
                    session.dispatchMemoryWarning(level);
                }
            }

            @Override
            public void onConfigurationChanged(Configuration newConfig) {
                if (BuildConfig.MIGO_API_SENSORS
                        && session != null && session.isValid() && boundActivity != null) {
                    String value = DisplayCompat.mapDeviceOrientationValue(boundActivity, newConfig);
                    if (!value.equals(lastOrientationEventValue)) {
                        lastOrientationEventValue = value;
                        NativeMethods.onDeviceOrientationChange(session.getSessionId(), value);
                    }
                }
            }

            @Override
            public void onLowMemory() {
                if (session != null && session.isValid()) {
                    session.dispatchMemoryWarning(15); // TRIM_MEMORY_RUNNING_CRITICAL
                }
            }
        };
        activity.registerComponentCallbacks(memoryCallback);
    }

    private void unregisterLifecycleCallbacks() {
        if (boundActivity != null && lifecycleCallbacks != null) {
            boundActivity.getApplication().unregisterActivityLifecycleCallbacks(lifecycleCallbacks);
            lifecycleCallbacks = null;
        }
        if (boundActivity != null && memoryCallback != null) {
            boundActivity.unregisterComponentCallbacks(memoryCallback);
            memoryCallback = null;
        }
        boundActivity = null;
    }

    private static Activity findActivity(Context context) {
        if (context instanceof Activity) {
            return (Activity) context;
        }
        if (context instanceof android.content.ContextWrapper) {
            return findActivity(((android.content.ContextWrapper) context).getBaseContext());
        }
        return null;
    }

    private RuntimeConfig ensureConfig() {
        if (config == null) {
            config = new RuntimeConfig.Builder(getContext()).build();
        }
        return config;
    }

    private Activity resolveActivity() {
        Activity activity = boundActivity;
        if (activity != null) {
            return activity;
        }
        return findActivity(getContext());
    }

    private void applyStartupOrientationIfNeeded(RuntimeConfig cfg, Activity activity) {
        String orientation = cfg != null ? cfg.getStartupOrientation() : null;

        if (orientation == null || activity == null) {
            orientationHelper.setTargetOrientation(null);
            return;
        }

        orientationHelper.setTargetOrientation(orientation);

        if (!orientation.equals(appliedStartupOrientation)) {
            DisplayCompat.setDeviceOrientation(activity, orientation);
            appliedStartupOrientation = orientation;
        }
    }

    private void notifySessionError(int errorCode, String message, boolean recoverable) {
        Log.e(TAG, "session error: code=" + errorCode + ", message=" + message);
        if (gameListener != null) {
            gameListener.onError(new MigoException(errorCode, message, null, recoverable));
        }
    }

    /**
     * Minimal implementation of ActivityLifecycleCallbacks with no-op defaults.
     */
    private static class EmptyActivityLifecycleCallbacks
            implements Application.ActivityLifecycleCallbacks {
        @Override public void onActivityCreated(Activity a, Bundle s) {}
        @Override public void onActivityStarted(Activity a) {}
        @Override public void onActivityResumed(Activity a) {}
        @Override public void onActivityPaused(Activity a) {}
        @Override public void onActivityStopped(Activity a) {}
        @Override public void onActivitySaveInstanceState(Activity a, Bundle s) {}
        @Override public void onActivityDestroyed(Activity a) {}
    }

    /**
     * Callback invoked when MigoGameView has created a GameSession.
     */
    public interface SessionCreatedListener {
        void onSessionCreated(GameSession session);
    }
}
