package com.migo.runtime;

import android.app.Activity;
import android.content.ComponentCallbacks2;
import android.content.Context;
import android.content.Intent;
import android.content.res.Configuration;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.graphics.Rect;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.widget.FrameLayout;

import com.migo.runtime.callback.GameSessionListener;
import com.migo.runtime.internal.NativeMethods;
import com.migo.runtime.internal.platform.DisplayCompat;
import com.migo.runtime.internal.platform.OrientationWaitHelper;

import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;

/**
 * Ready-to-use Activity for running a game with zero boilerplate.
 * <p>
 * All lifecycle management (pause, resume, surface, touch, memory warnings)
 * is handled internally. The host app only needs to launch this Activity.
 *
 * <h3>Usage</h3>
 * <pre>{@code
 * // Launch a game in one line:
 * MigoGameActivity.launch(context, "my-game", "game.js");
 *
 * // Or with custom config:
 * MigoGameActivity.launch(context, "my-game", "game.js",
 *     new RuntimeConfig.Builder(context)
 *         .setDebugEnabled(true)
 *         .build());
 * }</pre>
 *
 * <h3>Receiving game events</h3>
 * <p>
 * To receive game events, subclass this Activity and override
 * {@link #onCreateGameListener()}:
 * <pre>{@code
 * public class MyGameActivity extends MigoGameActivity {
 *     @Override
 *     protected GameSessionListener onCreateGameListener() {
 *         return new GameSessionListener() {
 *             @Override public void onGameReady() { ... }
 *             @Override public void onGameExit(int code) { finish(); }
 *             @Override public void onError(MigoException ex) { ... }
 *         };
 *     }
 * }
 * }</pre>
 *
 */
public class MigoGameActivity extends Activity
        implements SurfaceHolder.Callback, ComponentCallbacks2 {

    private static final String TAG = "MigoGameActivity";

    /** Intent extra key for game ID. */
    public static final String EXTRA_GAME_ID = "migo_game_id";
    /** Intent extra key for entry point file name. */
    public static final String EXTRA_ENTRY_POINT = "migo_entry_point";
    /** Internal extra key for pending RuntimeConfig token. */
    public static final String EXTRA_CONFIG_TOKEN = "migo_config_token";

    private static final ConcurrentHashMap<String, RuntimeConfig> sPendingConfigs =
            new ConcurrentHashMap<>();
    private static final ConcurrentHashMap<String, Long> sPendingConfigTimes =
            new ConcurrentHashMap<>();

    private GameSession session;
    /**
     * Whether {@code startGame} has already run for this activity.
     * <p>
     * Session existence used to stand in for this: the session was created at
     * the same moment the game was started, so "session != null" answered both
     * questions. A warm-started session exists from {@code onCreate}, long
     * before a Surface has arrived, so the two questions came apart and the
     * surface callbacks needed the one they actually meant.
     */
    private boolean gameStarted;
    private SurfaceView surfaceView;
    private final OrientationWaitHelper orientationHelper = new OrientationWaitHelper(TAG);
    private String lastOrientationEventValue;
    private String gameId;
    private String entryPoint;
    private RuntimeConfig config;

    /**
     * Launch a game with default configuration.
     *
     * @param context    Context to start from
     * @param gameId     Unique game identifier
     * @param entryPoint Entry point file (e.g., "game.js")
     */
    public static void launch(Context context, String gameId, String entryPoint) {
        launch(context, gameId, entryPoint, null);
    }

    /**
     * Launch a game with custom configuration.
     *
     * @param context    Context to start from
     * @param gameId     Unique game identifier
     * @param entryPoint Entry point file (e.g., "game.js")
     * @param config     Runtime configuration (null for defaults)
     */
    public static void launch(Context context, String gameId, String entryPoint,
                              RuntimeConfig config) {
        Intent intent = buildLaunchIntent(
                context,
                MigoGameActivity.class,
                gameId,
                entryPoint,
                config
        );
        context.startActivity(intent);
    }

    /**
     * Build a launch intent for MigoGameActivity (or subclasses).
     *
     * Subclasses can use this to launch themselves without relying on reflection.
     */
    protected static Intent buildLaunchIntent(
            Context context,
            Class<? extends MigoGameActivity> activityClass,
            String gameId,
            String entryPoint,
            RuntimeConfig config
    ) {
        Intent intent = new Intent(context, activityClass);
        intent.putExtra(EXTRA_GAME_ID, gameId);
        intent.putExtra(EXTRA_ENTRY_POINT, entryPoint);

        if (config != null) {
            // Clean up stale config tokens older than 30 seconds (e.g. Activity never started)
            long now = System.currentTimeMillis();
            sPendingConfigTimes.entrySet().removeIf(e -> {
                if (now - e.getValue() > 30_000) {
                    sPendingConfigs.remove(e.getKey());
                    return true;
                }
                return false;
            });

            String token = UUID.randomUUID().toString();
            sPendingConfigs.put(token, config);
            sPendingConfigTimes.put(token, now);
            intent.putExtra(EXTRA_CONFIG_TOKEN, token);
        }

        if (!(context instanceof Activity)) {
            intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        }
        return intent;
    }

    private final Handler mainHandler = new Handler(Looper.getMainLooper());
    private Runnable pendingInit;

    private static RuntimeConfig consumePendingConfig(Intent intent) {
        if (intent == null) {
            return null;
        }
        String token = intent.getStringExtra(EXTRA_CONFIG_TOKEN);
        if (token == null || token.isEmpty()) {
            return null;
        }
        sPendingConfigTimes.remove(token);
        return sPendingConfigs.remove(token);
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        // Extract launch parameters
        config = consumePendingConfig(getIntent());
        gameId = getIntent().getStringExtra(EXTRA_GAME_ID);
        entryPoint = getIntent().getStringExtra(EXTRA_ENTRY_POINT);
        if (gameId == null || entryPoint == null) {
            onLaunchFailed(ErrorCode.ERR_INVALID_GAME_ID, "Missing gameId or entryPoint");
            return;
        }
        if (config == null) {
            config = onCreateRuntimeConfig();
            if (config == null) {
                config = new RuntimeConfig.Builder(this).build();
            }
        }

        // Check runtime support
        MigoRuntime runtime = MigoRuntime.getInstance();
        if (!runtime.isDeviceSupported()) {
            onLaunchFailed(ErrorCode.ERR_NOT_SUPPORTED, "Device not supported");
            return;
        }

        applyStartupOrientation();
        applyStartupImmersiveMode();

        // Create surface view
        FrameLayout root = new FrameLayout(this);
        surfaceView = new SurfaceView(this);
        root.addView(surfaceView, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT,
                FrameLayout.LayoutParams.MATCH_PARENT));
        setContentView(root);

        // Set up surface callbacks
        surfaceView.getHolder().addCallback(this);

        // Set up touch handling
        surfaceView.setOnTouchListener((v, event) -> {
            if (session != null && session.isValid()) {
                return session.dispatchTouchEvent(event);
            }
            return false;
        });

        // Register for memory warnings
        registerComponentCallbacks(this);

        if (onWarmStartEngine()) {
            MigoRuntime.Result<GameSession> warm =
                    runtime.createSessionWarmSafe(this, config, gameId);
            if (warm.isFailure()) {
                Log.w(TAG, "warm start unavailable (code=" + warm.getErrorCode() + ", msg="
                        + warm.getErrorMessage() + "); starting at surfaceCreated instead");
            } else {
                session = warm.getValue();
                publishSession();
            }
        }
    }

    /**
     * Whether to start the engine here, in {@code onCreate}, instead of when the
     * Surface arrives. Off by default, and the default is the measured one.
     * <p>
     * Starting it here was measured on a reference Android device and it lost, on both
     * numbers it was meant to win: first frame 369 -> 401 ms and game-ready
     * 788 -> 838 ms. The ~150 ms between {@code onCreate} and
     * {@code surfaceCreated} is idle on the main thread but not on the CPU -- it
     * is process init, dex loading, layout and, for a landscape game, a window
     * rotation. Three more engine threads in that window take more from Android's
     * launch than the engine's head start gives back.
     * <p>
     * Override and return true only if your own measurement says otherwise on the
     * devices you ship to. If you want the head start without the contention, the
     * answer is not this flag but {@link MigoRuntime#createSessionWarm} called
     * genuinely early -- while the user is still choosing a game, not while the
     * activity that will run it is being laid out.
     *
     * @return false by default
     */
    protected boolean onWarmStartEngine() {
        return false;
    }

    /**
     * Hand a freshly created session to the subclass and its listener.
     *
     * <p>Shared by both routes to a session -- the warm start in
     * {@code onCreate} and the fallback create in
     * {@code attachSurfaceAndStart} -- so a subclass sees exactly the same
     * callbacks in the same order either way.
     *
     * @return false if the session-created callback threw and the launch was
     *         failed; true after the session and listener are published
     */
    private boolean publishSession() {
        lastOrientationEventValue = DisplayCompat.mapDeviceOrientationValue(
                this,
                getResources().getConfiguration()
        );
        try {
            onSessionCreated(session);
        } catch (Throwable t) {
            Log.e(TAG, "onSessionCreated failed", t);
            onLaunchFailed(ErrorCode.ERR_INIT_FAILED, "onSessionCreated threw: " + t.getMessage());
            return false;
        }
        GameSessionListener listener = onCreateGameListener();
        if (listener != null) {
            session.setListener(listener);
        }
        return true;
    }

    // ==================== SurfaceHolder.Callback ====================

    @Override
    public void surfaceCreated(SurfaceHolder holder) {
        Log.i(TAG, "surfaceCreated: frame=" + holder.getSurfaceFrame());
        // Once the game is running, a Surface callback is a recreate and nothing
        // more. Before that it is the event the launch has been waiting for --
        // including for a warm-started session, which exists here but has never
        // been given a Surface.
        if (gameStarted && session != null && session.isValid()) {
            orientationHelper.cancel();
            Rect frame = holder.getSurfaceFrame();
            session.updateSurface(holder.getSurface(), frame.width(), frame.height());
        } else if (orientationHelper.getTargetOrientation() != null) {
            Rect frame = holder.getSurfaceFrame();
            if (orientationHelper.surfaceMatches(frame.width(), frame.height())) {
                scheduleInitializeGame(holder);
            } else {
                Log.i(TAG, "surfaceCreated: deferring init until surface matches "
                        + orientationHelper.getTargetOrientation());
                orientationHelper.defer(holder, this::scheduleInitializeGame);
            }
        } else {
            scheduleInitializeGame(holder);
        }
    }

    @Override
    public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {
        Log.i(TAG, "surfaceChanged: format=" + format + ", size=" + width + "x" + height);
        if (gameStarted && session != null && session.isValid()) {
            orientationHelper.cancel();
            session.updateSurface(holder.getSurface(), width, height);
        } else {
            SurfaceHolder pending = orientationHelper.consumePending();
            if (pending != null && orientationHelper.surfaceMatches(width, height)) {
                Log.i(TAG, "surfaceChanged: surface matches, initializing game");
                scheduleInitializeGame(pending);
            } else if (pending != null) {
                orientationHelper.defer(pending, this::scheduleInitializeGame);
            }
        }
    }

    @Override
    public void surfaceDestroyed(SurfaceHolder holder) {
        Log.i(TAG, "surfaceDestroyed");
        cancelPendingInit();
        orientationHelper.cancel();
        if (session != null && session.isValid()) {
            session.onSurfaceDestroyed();
        }
        // Do NOT destroy the session. Surface is destroyed on onStop but
        // Activity is still alive. Cleanup happens in onDestroy.
    }

    // ==================== Activity Lifecycle ====================

    @Override
    protected void onPause() {
        super.onPause();
        Log.i(TAG, "onPause");
        if (session != null && session.isValid()) {
            session.pause();
        }
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "onResume");
        if (session != null && session.isValid()) {
            session.resume();
        }
    }

    @Override
    protected void onDestroy() {
        cancelPendingInit();
        orientationHelper.cancel();
        unregisterComponentCallbacks(this);
        if (session != null) {
            session.close();
            session = null;
        }
        super.onDestroy();
    }

    // ==================== ComponentCallbacks2 ====================

    @Override
    public void onTrimMemory(int level) {
        if (session != null && session.isValid()) {
            session.dispatchMemoryWarning(level);
        }
    }

    @Override
    public void onConfigurationChanged(Configuration newConfig) {
        super.onConfigurationChanged(newConfig);
        if (BuildConfig.MIGO_API_SENSORS && session != null && session.isValid()) {
            String value = DisplayCompat.mapDeviceOrientationValue(this, newConfig);
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

    // ==================== Hooks for subclasses ====================

    /**
     * Called when game launch fails. Override to show an error UI or report the failure.
     * Default implementation logs and finishes the activity.
     *
     * @param errorCode error code from {@link ErrorCode}
     * @param message   human-readable error description
     */
    protected void onLaunchFailed(int errorCode, String message) {
        Log.e(TAG, "Launch failed: [" + errorCode + "] " + message);
        finish();
    }

    /**
     * The configuration to run with when the launch did not carry one.
     *
     * <p>{@link #buildLaunchIntent} hands a config over through an in-process
     * table keyed by a token in the intent, which works only when whatever
     * started this activity is in this process. A game opened from a deep link,
     * a notification, a launcher shortcut or {@code am start} is not: the token
     * is absent, and such a launch silently ran on a default config, whatever
     * the host had configured everywhere else.
     *
     * <p>Override it to describe how your app runs games, and the same
     * description then applies however the activity was reached. The default
     * returns {@code null}, which keeps the previous behaviour exactly: a plain
     * {@code RuntimeConfig} built from this context.
     *
     * <p>Called from {@code onCreate}, before the surface exists, so it is also
     * the place to make sure the game's files are where the runtime will look
     * for them.
     *
     * @return the configuration to use, or {@code null} for the default
     */
    protected RuntimeConfig onCreateRuntimeConfig() {
        return null;
    }

    /**
     * Override to provide a custom game session listener.
     * <p>
     * Called during game initialization. Return null for no listener.
     *
     * @return A GameSessionListener, or null
     */
    protected GameSessionListener onCreateGameListener() {
        return null;
    }

    /**
     * Get the current game session.
     *
     * @return The active GameSession, or null if not initialized
     */
    protected GameSession getGameSession() {
        return session;
    }

    /**
     * Called immediately after session creation and before startGame.
     *
     * Override this to register host handlers that should be available before
     * game code starts running.
     */
    protected void onSessionCreated(GameSession session) {
    }

    // ==================== Private ====================

    private void initializeGame(SurfaceHolder holder) {
        orientationHelper.cancel();
        Log.i(TAG, "initializeGame: gameId=" + gameId + ", entry=" + entryPoint);

        if (session == null || !session.isValid()) {
            // No warm session: either the warm start failed above, or a
            // subclass tore this one down. Create it the original way, with the
            // Surface in hand.
            MigoRuntime.Result<GameSession> result = MigoRuntime.getInstance()
                    .createSessionSafe(this, holder.getSurface(), config, gameId);
            if (result.isFailure()) {
                Log.e(TAG, "initializeGame failed: code=" + result.getErrorCode()
                        + ", msg=" + result.getErrorMessage());
                onLaunchFailed(result.getErrorCode(), result.getErrorMessage());
                return;
            }
            session = result.getValue();
            if (!publishSession()) {
                return;
            }
        } else {
            // Warm session: it has been building its GPU stack since onCreate
            // and is waiting for exactly this. `updateSurface` installs a first
            // Surface by the same path it installs a replacement -- the engine
            // distinguishes them by whether a live one is already bound, not by
            // which call delivered it.
            Rect frame = holder.getSurfaceFrame();
            session.updateSurface(holder.getSurface(), frame.width(), frame.height());
        }

        // Start the game
        gameStarted = true;
        int startResult = session.startGameSafe(entryPoint);
        Log.i(TAG, "startGameSafe result=" + startResult);
        if (startResult != ErrorCode.SUCCESS) {
            onLaunchFailed(startResult, ErrorCode.getMessage(startResult));
        }
    }

    /**
     * Go full-screen before the surface exists, not after.
     * <p>
     * Immersive mode is on by default, and {@code createSession} applies it --
     * which is after the window has already been laid out with the system bars
     * and after the surface was created at that smaller size. Hiding the bars
     * then resizes the window, so every launch produced a second
     * {@code surfaceChanged} and made the engine tear down and rebuild its
     * GPU-side surface while the game was still starting. Measured on a reference Android device, the surface went 2235x1080 ->
     * 2340x1080 some 66 ms after the first
     * one, all of it on the path to first frame.
     * <p>
     * Applying the same flags here, before {@code setContentView}, means the
     * first surface is already the final one. {@code createSession} still calls
     * it for hosts that embed {@link MigoGameView} instead of subclassing this
     * activity; the operation is idempotent window state, so the second call
     * changes nothing.
     */
    /**
     * Start the session on the next main-thread message rather than inside the
     * surface callback.
     * <p>
     * {@code surfaceCreated} is delivered from the middle of a
     * {@code ViewRootImpl} traversal, and creating a session is not cheap: it
     * spawns the host thread and blocks until that thread has built the V8
     * isolate and the graphics stack. Measured on a reference Android device, that is ~114 ms
     * of the launch, and every millisecond of it is a millisecond the window
     * cannot finish its first draw -- the activity transition stalls and the
     * system reports the activity displayed that much later.
     * <p>
     * Nothing about the session needs to happen inside the traversal. Posting it
     * lets the traversal finish and draw, then does the same work one message
     * later. The game itself starts at most one frame later than before; the
     * window appears far sooner and the main thread is never held.
     */
    private void scheduleInitializeGame(SurfaceHolder holder) {
        cancelPendingInit();
        final SurfaceHolder target = holder;
        pendingInit = new Runnable() {
            @Override
            public void run() {
                pendingInit = null;
                if (isFinishing() || isDestroyed()) {
                    return;
                }
                initializeGame(target);
            }
        };
        mainHandler.post(pendingInit);
    }

    /** Drop a scheduled start whose surface or activity is going away. */
    private void cancelPendingInit() {
        if (pendingInit != null) {
            mainHandler.removeCallbacks(pendingInit);
            pendingInit = null;
        }
    }

    private void applyStartupImmersiveMode() {
        if (config != null && config.isImmersiveMode()) {
            DisplayCompat.enterImmersiveMode(this);
        }
    }

    private void applyStartupOrientation() {
        String orientation = config != null ? config.getStartupOrientation() : null;
        if (orientation == null) {
            orientationHelper.setTargetOrientation(null);
            return;
        }

        orientationHelper.setTargetOrientation(orientation);
        int result = DisplayCompat.setDeviceOrientation(this, orientation);
        Log.i(TAG, "applyStartupOrientation: " + orientation + ", result=" + result);
    }
}
