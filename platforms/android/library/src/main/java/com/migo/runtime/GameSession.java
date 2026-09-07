package com.migo.runtime;

import android.app.Activity;
import android.content.Context;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.view.MotionEvent;
import android.view.Surface;
import android.view.View;

import com.migo.runtime.callback.AdHandler;
import com.migo.runtime.callback.NavigationHandler;
import com.migo.runtime.callback.PaymentHandler;
import com.migo.runtime.callback.PermissionHandler;
import com.migo.runtime.callback.PermissionSink;
import com.migo.runtime.callback.SettingHandler;
import com.migo.runtime.callback.ShareHandler;
import com.migo.runtime.callback.AuthHandler;
import com.migo.runtime.callback.GameLogHandler;
import com.migo.runtime.callback.GameSessionListener;
import com.migo.runtime.callback.SubpackageHandler;
import com.migo.runtime.internal.ExclusiveDeviceArbiter;
import com.migo.runtime.internal.NativeExports;
import com.migo.runtime.internal.NativeMethods;
import com.migo.runtime.internal.RuntimeContext;
import com.migo.runtime.internal.RuntimeRegistry;
import com.migo.runtime.internal.ResourceCleanup;
import com.migo.runtime.internal.TerminalCleanupState;
import com.migo.runtime.internal.ThreadCheck;
import com.migo.runtime.internal.TouchEventHandler;
import com.migo.runtime.internal.VsyncScheduler;
import com.migo.runtime.internal.platform.AudioFocusManager;
import com.migo.runtime.internal.platform.DisplayCompat;
import com.migo.runtime.internal.util.Logger;

import java.io.Closeable;
import java.io.File;
import java.util.concurrent.atomic.AtomicReference;

/**
 * Represents an active game session.
 * <p>
 * A GameSession is created by {@link MigoRuntime#createSession} and manages the
 * lifecycle of a running game. Each session is isolated with its own file system
 * sandbox based on the game ID.
 *
 * <h3>Usage Example</h3>
 * <pre>{@code
 * // Create and start a game session
 * GameSession session = MigoRuntime.getInstance()
 *     .createSession(activity, surface, config, "my-game-id");
 *
 * session.setListener(listener);
 *
 * // Start the game from its code directory
 * session.startGame("game.js");  // Uses paths.getCodeDir()
 *
 * // Handle touch events
 * surfaceView.setOnTouchListener((v, event) -> {
 *     session.dispatchTouchEvent(event);
 *     return true;
 * });
 *
 * // In onPause
 * session.pause();
 *
 * // In onResume
 * session.resume();
 *
 * // In onDestroy
 * session.close();
 * }</pre>
 *
 * <h3>File System Sandbox</h3>
 * <p>
 * Each game has isolated directories accessible via {@link #getPaths()}:
 * <ul>
 *   <li>{@code /user} → User data (saves, preferences)</li>
 *   <li>{@code /cache} → Cache files</li>
 *   <li>{@code /code} → Game code (read-only)</li>
 *   <li>{@code /tmp} → Temporary files (cleared on close)</li>
 * </ul>
 *
 * <h3>Thread Safety</h3>
 * <p>
 * Session state and asynchronous native callbacks are thread-safe. Methods
 * that interact with Android UI objects are main-thread confined and enforce
 * that contract: game start, lifecycle transitions, Surface replacement,
 * touch dispatch, and {@link #close()}.
 *
 */
public final class GameSession implements Closeable {

    private static final String TAG = "GameSession";

    private final int sessionId;
    private final String gameId;
    private final RuntimeConfig config;
    private final Context context;
    private final GamePaths paths;
    private final TouchEventHandler touchHandler;
    private final AudioFocusManager audioFocusManager;
    private final VsyncScheduler vsyncScheduler;
    private final Handler mainHandler;
    private final Object lock = new Object();
    private final TerminalCleanupState terminalCleanup = new TerminalCleanupState();
    private boolean nativeShutdownComplete;

    private final AtomicReference<SessionState> state = new AtomicReference<>(SessionState.CREATED);
    private volatile boolean showDispatched = false;
    /**
     * Whether a Surface is currently attached. False from construction for a
     * warm-started session -- one created before its window existed -- until
     * {@link #updateSurface} delivers the first one.
     */
    private volatile boolean hasLiveSurface;

    private final long creationNanos = System.nanoTime();
    private volatile long startupTimeMs = -1;

    private DebugOverlayView debugOverlay;
    private boolean debugOverlayAttached = false;
    private ConsoleLogView consoleLogView;

    // Callbacks
    private volatile GameSessionListener listener;
    private volatile OnStateChangeListener stateChangeListener;
    private volatile MessageHandler messageHandler;

    /**
     * Create a new game session.
     * <p>
     * This constructor is package-private. Use {@link MigoRuntime#createSession} to create sessions.
     *
     * @param sessionId The native session ID
     * @param gameId    The unique game identifier
     * @param config    The runtime configuration
     * @param context   The context for system services
     * @param hasLiveSurface Whether a Surface was handed to the native session
     *                  at creation. False for a warm start, where the vsync
     *                  scheduler must not treat the session as presentable
     *                  until the first {@link #updateSurface}.
     */
    GameSession(int sessionId, String gameId, RuntimeConfig config, Context context,
                boolean hasLiveSurface) {
        this.hasLiveSurface = hasLiveSurface;
        this.sessionId = sessionId;
        this.gameId = gameId;
        this.config = config;
        this.context = context;
        this.paths = new GamePaths(config, gameId, sessionId);
        // Until this call existed the configured level was ignored on the Java
        // side entirely. Registered rather than assigned: a second session must
        // not be able to lower the level a live one asked for.
        Logger.registerSession(sessionId, config.getLogLevel());
        this.touchHandler = new TouchEventHandler(config.getDisplayDensity());
        this.audioFocusManager = BuildConfig.MIGO_API_MEDIA ? new AudioFocusManager(sessionId, context) : null;
        this.vsyncScheduler = new VsyncScheduler(sessionId);
        this.mainHandler = new Handler(Looper.getMainLooper());

        // Reclaim the temp directories of sessions that died without running their
        // own teardown, before this session's own directory exists. Ordering is
        // what makes it safe and what makes /tmp start empty: sessions are only
        // created on the main thread and are registered as they are created, so
        // every other live session is already registered here, while this one is
        // not -- so a directory left by a dead session that held this id is swept
        // rather than inherited.
        this.paths.sweepAbandonedTemp(RuntimeRegistry::contains);

        // Ensure game directories exist
        this.paths.ensureDirectories();

        // Start listening for audio focus changes
        if (audioFocusManager != null) {
            audioFocusManager.start();
        }

        this.vsyncScheduler.setSurfaceReady(hasLiveSurface);

        // Start Choreographer-driven VSync immediately for the current live surface
        this.vsyncScheduler.start();

        // Create debug overlay and console log viewer if debug mode is enabled
        if (config.isDebugEnabled()) {
            this.debugOverlay = new DebugOverlayView(context, sessionId);
            this.debugOverlay.startMonitoring();
            this.consoleLogView = new ConsoleLogView(context, sessionId);
        }

        // Register session for lifecycle callbacks from native
        NativeExports.registerSession(sessionId, this);

        // R1 bootstrap: native render initialization may request its first
        // one-shot before this Java wrapper is registered (notably through the
        // Context overload, which may be created off the UI thread). That early
        // request is intentionally a no-op because no live session exists yet.
        // Re-request once after publication so the initial dirty frame cannot
        // remain stranded. NativeExports hops to the main thread and the Java
        // scheduler coalesces this with any request already in flight.
        NativeExports.requestVsync(sessionId);

        // Register for native engine error callbacks (OOM, ANR, Panic, Timeout)
        NativeExports.registerErrorCallback(sessionId, new NativeExports.NativeErrorCallback() {
            @Override
            public void onNativeError(int errorCode, String message, String detail) {
                String fullMessage = detail != null && !detail.isEmpty()
                        ? message + " — " + detail
                        : message;
                notifyError(errorCode, fullMessage, ErrorCode.isRecoverable(errorCode));
            }

            @Override
            public void onExit() {
                notifyGameExit(0);
            }
        });
    }

    // ==================== Getters ====================

    /**
     * Get the session ID.
     *
     * @return The native session ID
     */
    public int getSessionId() {
        return sessionId;
    }

    /**
     * Get the game ID.
     *
     * @return The unique game identifier
     */
    public String getGameId() {
        return gameId;
    }

    /**
     * Get the game paths manager.
     * <p>
     * Provides access to isolated directories for this game.
     *
     * @return The GamePaths instance
     */
    public GamePaths getPaths() {
        return paths;
    }

    /**
     * Check if this session is still valid (not destroyed).
     *
     * @return true if the session is valid
     */
    public boolean isValid() {
        return state.get() != SessionState.DESTROYED;
    }

    /**
     * Check if a game has been started.
     *
     * @return true if startGame() has been called successfully
     */
    public boolean isGameStarted() {
        SessionState s = state.get();
        return s == SessionState.RUNNING || s == SessionState.PAUSED;
    }

    /**
     * Returns the current lifecycle state of this session.
     */
    public SessionState getState() {
        return state.get();
    }

    /**
     * Set a listener to observe lifecycle state changes.
     * Callbacks are delivered on the main thread.
     * Pass null to remove the listener.
     */
    public void setOnStateChangeListener(OnStateChangeListener listener) {
        this.stateChangeListener = listener;
    }

    /**
     * Get the debug overlay view, if debug mode is enabled.
     * <p>
     * The overlay is automatically attached as a WindowManager panel on the
     * first {@link #updateSurface} call, floating above the game SurfaceView.
     * No manual {@code addView()} is needed.
     *
     * @return The debug overlay view, or null if debug mode is disabled
     */
    public DebugOverlayView getDebugOverlay() {
        return debugOverlay;
    }

    /**
     * Returns a snapshot of current engine performance metrics.
     * Returns null if the session is not running or stats are unavailable.
     * <p>
     * This method polls the native engine and parses the binary stats protocol.
     * Safe to call from any thread, but avoid calling more than once per second
     * to minimize overhead.
     */
    public PerformanceSnapshot getPerformanceSnapshot() {
        if (state.get() == SessionState.DESTROYED) return null;
        byte[] data = NativeMethods.getDebugStats(sessionId);
        return PerformanceSnapshot.fromStatsPacket(data);
    }

    // ==================== Game Control ====================

    /**
     * Start the game.
     * <p>
     * The native layer will:
     * <ul>
     *   <li>Create isolated directories for this game based on gameId</li>
     *   <li>Set up the virtual file system with proper permissions</li>
     *   <li>Load and execute the entry point module</li>
     * </ul>
     * <p>
     * Before calling this method:
     * <ul>
     *   <li>For a first install, deploy the complete signed game tree to
     *       {@code paths.getCodeDir()}</li>
     *   <li>For an update, stop every session for this game, serialize deployment
     *       against new starts, and publish a complete sibling tree with a
     *       recoverable transaction; a previously verified active tree is
     *       sealed read-only and must never be modified in place</li>
     *   <li>Complete any interrupted deployment recovery before starting</li>
     *   <li>Verify the entry point file exists</li>
     * </ul>
     *
     * @param entryPoint Entry point file (e.g., "game.js", "main.js")
     * @throws MigoException if the session is destroyed or game fails to start
     */
    public void startGame(String entryPoint) {
        ThreadCheck.ensureMainThread();
        ensureNotDestroyed();

        if (entryPoint == null || entryPoint.isEmpty()) {
            throw new MigoException(ErrorCode.ERR_ENTRY_NOT_FOUND,
                    ErrorCode.getMessage(ErrorCode.ERR_ENTRY_NOT_FOUND) + ": entryPoint is null or empty");
        }

        // Optional: Validate code directory and entry point exist (for better error messages)
        File codeDir = paths.getCodeDir();
        if (!codeDir.exists() || !codeDir.isDirectory()) {
            throw new MigoException(ErrorCode.ERR_CODE_DIR_NOT_FOUND,
                    ErrorCode.getMessage(ErrorCode.ERR_CODE_DIR_NOT_FOUND) + ": Code directory not found");
        }

        File entry = new File(codeDir, entryPoint);
        if (!entry.exists() || !entry.isFile()) {
            throw new MigoException(ErrorCode.ERR_ENTRY_NOT_FOUND,
                    ErrorCode.getMessage(ErrorCode.ERR_ENTRY_NOT_FOUND) + ": Entry point not found");
        }

        // Ensure launch options are available before entry execution.
        synchronized (lock) {
            dispatchShowIfNeeded();
        }

        // Native layer handles path generation from gameId
        int result = NativeMethods.modMain(sessionId, gameId, entryPoint);
        if (result != 0) {
            throw new MigoException(ErrorCode.ERR_JS_EXECUTION,
                    ErrorCode.getMessage(ErrorCode.ERR_JS_EXECUTION) + ": Native modMain returned " + result);
        }

        SessionState prev = state.getAndSet(SessionState.RUNNING);
        Log.d("MigoSession", "Session " + sessionId + " state: " + prev + " -> RUNNING");
        notifyStateChange(prev, SessionState.RUNNING);
        NativeExports.resumePowerSensitiveManagers(sessionId);
    }

    /**
     * Start the game (non-throwing version).
     *
     * @param entryPoint Entry point file (e.g., "game.js")
     * @return {@link ErrorCode#SUCCESS} on success, or an error code
     */
    public int startGameSafe(String entryPoint) {
        try {
            startGame(entryPoint);
            return ErrorCode.SUCCESS;
        } catch (MigoException e) {
            return e.getErrorCode();
        } catch (Exception e) {
            return ErrorCode.ERR_JS_EXECUTION;
        }
    }

    // ==================== Host <-> JS Communication ====================

    /**
     * Handler for messages sent from game JavaScript to the host app.
     */
    public interface MessageHandler {
        /**
         * Called when the game sends a message to the host.
         * Called on the main thread.
         *
         * @param type    message type identifier
         * @param payload JSON string payload (may be null)
         */
        void onMessage(String type, String payload);
    }

    /**
     * Execute JavaScript code in the game's V8 runtime.
     * <p>
     * The script is evaluated asynchronously on the host thread. This method
     * returns immediately. The script runs in the global scope of the game.
     * <p>
     * Example: {@code session.evaluateJavaScript("console.log('hello from host')");}
     * <p>
     * To receive data back from JS, register a custom handler via
     * {@link #setMessageHandler} and have JS call {@code migo.sendToHost(type, payload)}.
     *
     * @param script JavaScript source code to evaluate
     * @throws IllegalStateException if the session is destroyed
     */
    public void evaluateJavaScript(String script) {
        ensureNotDestroyed();
        if (script == null || script.isEmpty()) return;
        NativeMethods.executeScript(sessionId, script);
    }

    /**
     * Set a handler to receive messages from the game JavaScript.
     * <p>
     * In the game JS, call {@code migo.sendToHost(type, payload)} to send messages.
     * Pass null to remove the handler.
     *
     * @param handler The handler to receive messages, or null to remove
     */
    public void setMessageHandler(MessageHandler handler) {
        this.messageHandler = handler;
        NativeExports.registerMessageHandler(sessionId, handler);
    }

    // ==================== Lifecycle ====================

    /**
     * Pause the game (call when activity goes to background).
     */
    public void pause() {
        ThreadCheck.ensureMainThread();
        synchronized (lock) {
            if (state.get() == SessionState.DESTROYED) return;
            transitionState(SessionState.RUNNING, SessionState.PAUSED);
            NativeExports.suspendPowerSensitiveManagers(sessionId);
            vsyncScheduler.stop();
            if (debugOverlay != null) {
                debugOverlay.stopMonitoring();
                debugOverlay.detachFromWindow();
                debugOverlayAttached = false;
            }
            if (consoleLogView != null) {
                consoleLogView.stopPolling();
                consoleLogView.detach();
            }
            dispatchHideIfNeeded();
        }
        GameSessionListener l = listener;
        if (l != null) {
            l.onPaused();
        }
    }

    /**
     * Resume the game (call when activity comes to foreground).
     */
    public void resume() {
        ThreadCheck.ensureMainThread();
        synchronized (lock) {
            if (state.get() == SessionState.DESTROYED) return;
            transitionState(SessionState.PAUSED, SessionState.RUNNING);
            vsyncScheduler.setSurfaceReady(hasLiveSurface);
            vsyncScheduler.start();
            if (debugOverlay != null) {
                debugOverlay.startMonitoring();
                if (!debugOverlayAttached) {
                    tryAttachDebugOverlay();
                }
            }
            dispatchShowIfNeeded();
            NativeExports.resumePowerSensitiveManagers(sessionId);
            // Re-request audio focus in case it was permanently lost (e.g.
            // after a phone call). This ensures onAudioInterruptionEnd fires.
            if (audioFocusManager != null) {
                audioFocusManager.requestFocusIfNeeded();
            }
        }
        GameSessionListener l = listener;
        if (l != null) {
            l.onResumed();
        }
    }

    /**
     * Restart the game.
     */
    public void restart() {
        synchronized (lock) {
            if (state.get() == SessionState.DESTROYED) return;
            NativeMethods.onRestart(sessionId);
        }
    }

    /**
     * Update the rendering surface.
     * <p>
     * Call this when the surface is recreated (e.g., after configuration change).
     *
     * @param surface The new Surface object
     */
    public void updateSurface(Surface surface) {
        ThreadCheck.ensureMainThread();
        updateSurface(surface, -1, -1);
    }

    /**
     * Update the rendering surface with explicit buffer dimensions.
     *
     * @param surface The new Surface object
     * @param width   Surface buffer width in physical pixels
     * @param height  Surface buffer height in physical pixels
     */
    public void updateSurface(Surface surface, int width, int height) {
        ThreadCheck.ensureMainThread();
        if (surface == null) {
            throw new MigoException(ErrorCode.ERR_INVALID_SURFACE,
                    ErrorCode.getMessage(ErrorCode.ERR_INVALID_SURFACE));
        }
        synchronized (lock) {
            if (state.get() == SessionState.DESTROYED) return;
            Log.i(TAG, "updateSurface: session=" + sessionId + ", valid=" + surface.isValid()
                    + ", size=" + width + "x" + height);
            final float density = DisplayCompat.getDensity(context);
            touchHandler.updateDensity(density);
            hasLiveSurface = surface.isValid();
            NativeMethods.updateSurface(sessionId, surface, width, height, density);
            vsyncScheduler.setSurfaceReady(hasLiveSurface);

            // Auto-attach debug overlay as a WindowManager panel on first surface update.
            // At this point the Activity window is guaranteed to have a valid token.
            if (debugOverlay != null && !debugOverlayAttached) {
                tryAttachDebugOverlay();
            }
        }
    }

    /**
     * Notify the session that the current rendering surface has been destroyed.
     */
    public void onSurfaceDestroyed() {
        ThreadCheck.ensureMainThread();
        synchronized (lock) {
            if (state.get() == SessionState.DESTROYED) return;
            hasLiveSurface = false;
            vsyncScheduler.setSurfaceReady(false);
            NativeMethods.onSurfaceDestroyed(sessionId);
        }
    }

    /**
     * Destroy this session and release all resources.
     * <p>
     * After calling this method, the session cannot be used anymore.
     * Successful cleanup is idempotent. If cleanup fails, a later explicit call
     * retries the retained resources.
     * <p>
     * Temporary files are automatically cleaned up.
     */
    @Override
    public void close() {
        ThreadCheck.ensureMainThread();
        SessionState prev;
        boolean firstClose;
        synchronized (lock) {
            prev = state.getAndSet(SessionState.DESTROYED);
            firstClose = prev != SessionState.DESTROYED;
            if (firstClose) {
                Log.d("MigoSession", "Session " + sessionId + " state: "
                        + prev + " -> DESTROYED");
            }
        }
        if (firstClose) {
            notifyStateChange(prev, SessionState.DESTROYED);
        }

        TerminalCleanupState.Result result = terminalCleanup.attempt(
                () -> ResourceCleanup.runAll(
                        () -> NativeExports.suspendPowerSensitiveManagers(sessionId),
                        () -> {
                            vsyncScheduler.setSurfaceReady(false);
                            vsyncScheduler.stop();
                        },
                        () -> {
                            if (debugOverlay != null) {
                                debugOverlay.stopMonitoring();
                                debugOverlay.detachFromWindow();
                            }
                        },
                        () -> {
                            if (consoleLogView != null) consoleLogView.detach();
                        },
                        () -> {
                            if (audioFocusManager != null) audioFocusManager.stop();
                        },
                        () -> NativeExports.closePermissionOperations(sessionId),
                        () -> NativeExports.destroyAllManagers(sessionId),
                        // Last, and unconditional: a session that dies holding the
                        // camera or microphone must not keep every other game off it
                        // for the life of the process. This cannot rely on each
                        // manager having released cleanly, because a failed release
                        // may be the reason this session is being torn down.
                        () -> ExclusiveDeviceArbiter.releaseAll(sessionId)),
                this::shutdownNativeOnce,
                () -> RuntimeRegistry.unregister(sessionId),
                paths::cleanupTemp,
                () -> Logger.unregisterSession(sessionId),
                () -> NativeExports.unregisterSession(sessionId));

        RuntimeException cleanupFailure = result.failure();
        if (cleanupFailure != null) {
            Log.e(TAG, "terminal_cleanup_failed session=" + sessionId
                    + " retryable=true", cleanupFailure);
            notifyError(ErrorCode.ERR_CLEANUP_FAILED,
                    ErrorCode.getMessage(ErrorCode.ERR_CLEANUP_FAILED)
                            + ": " + cleanupFailure.getMessage(),
                    false);
        }

        if (firstClose) {
            GameSessionListener l = listener;
            if (l != null) {
                try {
                    l.onDestroyed();
                } catch (RuntimeException listenerFailure) {
                    Log.e(TAG, "session_destroy_listener_failed session=" + sessionId,
                            listenerFailure);
                }
            }
        }
    }

    private void shutdownNativeOnce() {
        synchronized (lock) {
            if (nativeShutdownComplete) return;
        }
        if (!NativeMethods.shutdown(sessionId)) {
            throw new IllegalStateException("native shutdown/join failed");
        }
        synchronized (lock) {
            nativeShutdownComplete = true;
        }
    }

    /**
     * Alias for {@link #close()}.
     */
    public void destroy() {
        close();
    }

    // ==================== Memory Warning ====================

    /**
     * Forward a memory warning to the game session.
     * <p>
     * Call this from your Activity's {@code onTrimMemory} or Application's
     * {@code onTrimMemory} callback to notify the game of memory pressure.
     *
     * <pre>{@code
     * @Override
     * public void onTrimMemory(int level) {
     *     super.onTrimMemory(level);
     *     if (session != null && session.isValid()) {
     *         session.dispatchMemoryWarning(level);
     *     }
     * }
     * }</pre>
     *
     * @param level Android ComponentCallbacks2 trim memory level
     *              (e.g., TRIM_MEMORY_RUNNING_MODERATE=5, TRIM_MEMORY_RUNNING_LOW=10,
     *              TRIM_MEMORY_RUNNING_CRITICAL=15)
     */
    public void dispatchMemoryWarning(int level) {
        synchronized (lock) {
            if (state.get() == SessionState.DESTROYED) return;
            NativeMethods.onMemoryWarning(sessionId, level);
        }
    }

    // ==================== Input ====================

    /**
     * Dispatch a touch event to the game.
     * <p>
     * This method must be called synchronously on the main thread while the
     * {@link MotionEvent} is valid. That is the thread used by Android Views
     * and gives the reusable direct input buffer a single writer without a
     * monitor on the 60-120 Hz input path.
     *
     * @param event The MotionEvent from the view
     * @return true if the event was handled
     * @throws IllegalStateException if called off the main thread
     */
    public boolean dispatchTouchEvent(MotionEvent event) {
        ThreadCheck.ensureMainThread();
        if (event == null) return false;
        if (state.get() == SessionState.DESTROYED) return false;
        return touchHandler.dispatch(sessionId, event);
    }

    // ==================== Callback ====================

    /**
     * Set or clear the auth handler for this session.
     * <p>
     * This handler backs JS-side {@code migo.login()} and {@code migo.checkSession()}.
     * Call this before {@link #startGame(String)} for best compatibility.
     *
     * @param handler host auth handler, or null to clear
     */
    public void setAuthHandler(AuthHandler handler) {
        synchronized (lock) {
            if (state.get() == SessionState.DESTROYED) return;
            NativeExports.setAuthHandler(sessionId, handler);
        }
    }

    /**
     * Set or clear the permission handler for this session.
     * <p>
     * You decide what the game may do. With no handler installed every scope is
     * denied: {@code migo.getSetting()} reports nothing granted and capability
     * calls fail with {@code auth deny}. That is deliberate — your app may hold
     * the camera permission for its own features, and without this a game would
     * reach it under your grant with nobody asked about that game.
     * <p>
     * Call this before {@link #startGame(String)} for best compatibility.
     *
     * @param handler host permission handler, or null to clear
     */
    public void setPermissionHandler(PermissionHandler handler) {
        synchronized (lock) {
            if (state.get() == SessionState.DESTROYED) return;
            NativeExports.setPermissionHandler(sessionId, handler);
        }
    }

    /**
     * Set or clear the ad handler for this session.
     * <p>
     * Bridge this to your ad SDK (Pangle, GDT, Kuaishou Union, ...). The
     * runtime links no ad SDK and decides nothing about whether an advert was
     * watched -- with no handler installed, content's ad callbacks still fire
     * but no advert is shown and incentivised video reports an unfinished view.
     * <p>
     * Call this before {@link #startGame(String)} for best compatibility.
     *
     * @param handler host ad handler, or null to clear
     */
    public void setAdHandler(AdHandler handler) {
        synchronized (lock) {
            if (state.get() == SessionState.DESTROYED) return;
            NativeExports.setAdHandler(sessionId, handler);
        }
    }

    /**
     * Set or clear the setting handler for this session.
     * <p>
     * Backs {@code migo.openSetting()}, which content calls to send the user
     * somewhere a refused permission can be changed. The standing decisions live
     * with you — see {@link PermissionSink#setScope} — so the screen that edits
     * them does too. With no handler installed the call fails as not supported.
     * <p>
     * Call this before {@link #startGame(String)} for best compatibility.
     *
     * @param handler host setting handler, or null to clear
     */
    public void setSettingHandler(SettingHandler handler) {
        synchronized (lock) {
            if (state.get() == SessionState.DESTROYED) return;
            NativeExports.setSettingHandler(sessionId, handler);
        }
    }

    /**
     * Set or clear the share handler for this session.
     * <p>
     * Bridge this to your own share surface. The runtime links no share SDK and
     * holds no social graph — with no handler installed
     * {@code migo.shareAppMessage()} fails as not supported rather than stalling.
     * <p>
     * Call this before {@link #startGame(String)} for best compatibility.
     *
     * @param handler host share handler, or null to clear
     */
    public void setShareHandler(ShareHandler handler) {
        synchronized (lock) {
            if (state.get() == SessionState.DESTROYED) return;
            NativeExports.setShareHandler(sessionId, handler);
        }
    }

    /**
     * Set or clear the navigation handler for this session.
     * <p>
     * Covers the two ways content asks to leave the game: another mini program,
     * and your support channel. Whether either is allowed is your decision. With
     * no handler installed both fail as not supported.
     * <p>
     * Call this before {@link #startGame(String)} for best compatibility.
     *
     * @param handler host navigation handler, or null to clear
     */
    public void setNavigationHandler(NavigationHandler handler) {
        synchronized (lock) {
            if (state.get() == SessionState.DESTROYED) return;
            NativeExports.setNavigationHandler(sessionId, handler);
        }
    }

    /**
     * Set or clear the payment handler for this session.
     * <p>
     * Bridge this to your billing integration. The runtime holds no merchant
     * credentials and never decides that a purchase succeeded — with no handler
     * installed {@code migo.checkIsSupportMidasPayment()} reports no payment
     * channel, so well-behaved content never opens a store it cannot transact
     * in.
     * <p>
     * Call this before {@link #startGame(String)} for best compatibility.
     *
     * @param handler host payment handler, or null to clear
     */
    public void setPaymentHandler(PaymentHandler handler) {
        synchronized (lock) {
            if (state.get() == SessionState.DESTROYED) return;
            NativeExports.setPaymentHandler(sessionId, handler);
        }
    }

    /**
     * Set or clear the game log handler for this session.
     * <p>
     * This handler receives game analytics data reported by JS.
     * When no handler is set,
     * log entries are written to Android logcat.
     *
     * @param handler host log handler, or null to revert to logcat default
     */
    public void setGameLogHandler(GameLogHandler handler) {
        synchronized (lock) {
            if (state.get() == SessionState.DESTROYED) return;
            NativeExports.setGameLogHandler(sessionId, handler);
        }
    }

    /**
     * Set or clear the subpackage download handler for this session.
     * <p>
     * This handler backs JS-side {@code loadSubpackage()} and
     * {@code preDownloadSubpackage()} when local files are not available.
     * When no handler is set, download requests fail and {@code loadSubpackage()}
     * falls back to executing local files if present.
     * <p>
     * Call this before {@link #startGame(String)} for best compatibility.
     *
     * @param handler host download handler, or null to clear
     */
    public void setSubpackageHandler(SubpackageHandler handler) {
        synchronized (lock) {
            if (state.get() == SessionState.DESTROYED) return;
            NativeExports.setSubpackageHandler(sessionId, handler);
        }
    }

    /**
     * Set the listener for session events.
     *
     * @param listener The listener, or null to remove
     */
    public void setListener(GameSessionListener listener) {
        this.listener = listener;
    }

    // ==================== Internal callbacks from native ====================

    /**
     * @hide Called from native code via NativeExports.onGameReady (potentially from a non-UI thread).
     * Records startup timing, updates the debug overlay, then posts the listener
     * callback to the main thread with double-check to handle session destruction
     * between post and dispatch.
     */
    public void notifyGameReady() {
        startupTimeMs = (System.nanoTime() - creationNanos) / 1_000_000;
        if (debugOverlay != null) {
            debugOverlay.setStartupTimeMs(startupTimeMs);
        }
        GameSessionListener l = listener;
        if (l == null) return;
        mainHandler.post(() -> {
            GameSessionListener l2 = listener;
            if (l2 != null && state.get() != SessionState.DESTROYED) {
                l2.onGameReady();
            }
        });
    }

    /**
     * @hide R1: arm exactly one Choreographer frame. Called from
     * {@link com.migo.runtime.internal.NativeExports#requestVsync(int)} on the
     * main thread when the native render thread / RAF loop has frame demand.
     * Idempotent: the underlying scheduler coalesces duplicate requests.
     */
    public void requestVsyncFrame() {
        ThreadCheck.ensureMainThread();
        synchronized (lock) {
            if (state.get() == SessionState.DESTROYED) return;
            vsyncScheduler.requestFrame();
        }
    }

    /** @hide Called from native code (potentially from a non-UI thread). */
    void notifyGameExit(int exitCode) {
        GameSessionListener l = listener;
        if (l == null) return;
        mainHandler.post(() -> {
            GameSessionListener l2 = listener;
            if (l2 != null) {
                l2.onGameExit(exitCode);
            }
        });
    }

    /** @hide Called from native code (potentially from a non-UI thread). */
    void notifyError(int errorCode, String message, boolean recoverable) {
        GameSessionListener l = listener;
        if (l == null) return;
        MigoException ex = new MigoException(errorCode, message, null, recoverable);
        mainHandler.post(() -> {
            GameSessionListener l2 = listener;
            if (l2 != null) {
                l2.onError(ex);
            }
        });
    }

    /**
     * @hide Called from {@link com.migo.runtime.internal.NativeExports#onSurfaceLost}
     * on the main thread when the engine retired a Surface it could not present to.
     * <p>
     * Reports and changes nothing else, and both halves of that are deliberate.
     * <p>
     * It first cleared {@code hasLiveSurface} and disarmed vsync, which was a race with
     * the shape this whole area exists to remove. The report is posted to the main thread
     * from a render thread, so an {@code updateSurface} for a replacement can run in
     * between — and the callback would then have declared the replacement dead. Nothing
     * here can tell the two apart: the engine names the generation it lost, and this side
     * has never been told the generation of what it holds.
     * <p>
     * It also did not need to. The state has one truthful source, which is the app's
     * answer: {@link #updateSurface(Surface)} sets it for a replacement,
     * {@link #onSurfaceDestroyed()} sets it for a teardown, and until one of them arrives
     * the engine's own {@code SurfaceSystem} has already stopped asking for frames — a
     * retained request winds down rather than pumping an empty renderer. So this is the C
     * boundary's contract exactly: the engine marks its own attachment lost and tells the
     * host; the host decides.
     * <p>
     * No automatic re-attach either, for the reason the C boundary gives: this session
     * does not hold the Surface, and one that is genuinely gone would make retrying a loop.
     */
    public void notifySurfaceLost(long generation, int reason) {
        ThreadCheck.ensureMainThread();
        if (state.get() == SessionState.DESTROYED) return;
        Log.w(TAG, "surface lost: session=" + sessionId + ", generation=" + generation
                + ", reason=" + reason);
        GameSessionListener l = listener;
        if (l != null) {
            l.onSurfaceLost(reason);
        }
    }

    // ==================== Debug ====================

    /**
     * Enable or disable the debug overlay at runtime.
     * <p>
     * Must be called on the main thread.
     *
     * @param enabled true to show debug overlay, false to hide it
     * @hide
     */
    public void setDebugEnabled(boolean enabled) {
        synchronized (lock) {
            if (state.get() == SessionState.DESTROYED) return;
            if (enabled) {
                if (debugOverlay == null) {
                    RuntimeContext ctx = RuntimeRegistry.get(sessionId);
                    if (ctx == null) return;
                    Context context = ctx.getActivity();
                    if (context == null) return;
                    debugOverlay = new DebugOverlayView(context, sessionId);
                    debugOverlay.startMonitoring();
                    consoleLogView = new ConsoleLogView(context, sessionId);
                    tryAttachDebugOverlay();
                }
            } else {
                if (debugOverlay != null) {
                    debugOverlay.stopMonitoring();
                    debugOverlay.detachFromWindow();
                    debugOverlay = null;
                    debugOverlayAttached = false;
                }
                if (consoleLogView != null) {
                    consoleLogView.stopPolling();
                    consoleLogView.detach();
                    consoleLogView = null;
                }
            }
        }
    }

    // ==================== Helpers ====================

    private void tryAttachDebugOverlay() {
        RuntimeContext ctx = RuntimeRegistry.get(sessionId);
        if (ctx == null) return;
        Activity activity = ctx.getActivity();
        if (activity == null || activity.isFinishing()) return;
        View decor = activity.getWindow().getDecorView();
        if (decor.getWindowToken() == null) return;
        debugOverlay.attachToWindow(decor);
        if (consoleLogView != null) {
            consoleLogView.attachButton(decor);
        }
        debugOverlayAttached = true;
    }

    private void dispatchShowIfNeeded() {
        if (!showDispatched) {
            NativeMethods.onShow(sessionId);
            showDispatched = true;
        }
    }

    private void dispatchHideIfNeeded() {
        if (showDispatched) {
            NativeMethods.onHide(sessionId);
            showDispatched = false;
        }
    }

    private void ensureNotDestroyed() {
        if (state.get() == SessionState.DESTROYED) {
            throw new MigoException(ErrorCode.ERR_SESSION_DESTROYED,
                    ErrorCode.getMessage(ErrorCode.ERR_SESSION_DESTROYED));
        }
    }

    private boolean transitionState(SessionState expected, SessionState next) {
        boolean ok = state.compareAndSet(expected, next);
        if (ok) {
            Log.d("MigoSession", "Session " + sessionId + " state: " + expected + " -> " + next);
            notifyStateChange(expected, next);
        }
        return ok;
    }

    private void notifyStateChange(SessionState oldState, SessionState newState) {
        OnStateChangeListener l = stateChangeListener;
        if (l != null) {
            mainHandler.post(() -> {
                OnStateChangeListener l2 = stateChangeListener;
                if (l2 != null) {
                    l2.onStateChanged(this, oldState, newState);
                }
            });
        }
    }

    @Override
    public String toString() {
        return "GameSession{" +
                "sessionId=" + sessionId +
                ", gameId='" + gameId + '\'' +
                ", state=" + state.get() +
                '}';
    }
}
