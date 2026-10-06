package com.migo.runtime;

import android.app.Activity;
import android.content.Context;
import android.os.Build;
import android.util.Log;
import android.view.Surface;

import com.migo.runtime.internal.AppContext;
import com.migo.runtime.internal.NativeMethods;
import com.migo.runtime.internal.RuntimeContext;
import com.migo.runtime.internal.RuntimeRegistry;
import com.migo.runtime.internal.ThreadCheck;
import com.migo.runtime.internal.platform.DisplayCompat;

/**
 * Main entry point for the Migo Runtime SDK.
 * <p>
 * The Migo Runtime is a lightweight JavaScript game runtime for Android. It provides
 * a high-performance environment for running JavaScript-based games with Canvas 2D
 * and WebGL rendering support.
 *
 * <h3>Quick Start</h3>
 * <pre>{@code
 * // In your Activity or Fragment
 * public class GameActivity extends Activity {
 *     private GameSession session;
 *
 *     @Override
 *     protected void onCreate(Bundle savedInstanceState) {
 *         super.onCreate(savedInstanceState);
 *
 *         // Initialize the runtime
 *         MigoRuntime runtime = MigoRuntime.getInstance();
 *
 *         // Create configuration
 *         RuntimeConfig config = new RuntimeConfig.Builder(this)
 *             .setDebugEnabled(BuildConfig.DEBUG)
 *             .build();
 *
 *         // Create a surface view for rendering
 *         SurfaceView surfaceView = new SurfaceView(this);
 *         setContentView(surfaceView);
 *
 *         surfaceView.getHolder().addCallback(new SurfaceHolder.Callback() {
 *             @Override
 *             public void surfaceCreated(SurfaceHolder holder) {
 *                 // Create session when surface is ready
 *                 session = runtime.createSession(
 *                     GameActivity.this,
 *                     holder.getSurface(),
 *                     config,
 *                     "my-game-id"
 *                 );
 *
 *                 // Start the game
 *                 // Place game code into: session.getPaths().getCodeDir()
 *                 session.startGame("game.js");
 *             }
 *
 *             @Override
 *             public void surfaceChanged(SurfaceHolder holder, int f, int w, int h) {
 *                 if (session != null) {
 *                     session.updateSurface(holder.getSurface(), w, h);
 *                 }
 *             }
 *
 *             @Override
 *             public void surfaceDestroyed(SurfaceHolder holder) {
 *                 if (session != null) {
 *                     session.onSurfaceDestroyed();
 *                 }
 *             }
 *         });
 *
 *         // Handle touch events
 *         surfaceView.setOnTouchListener((v, event) -> {
 *             if (session != null) {
 *                 return session.dispatchTouchEvent(event);
 *             }
 *             return false;
 *         });
 *     }
 *
 *     @Override
 *     protected void onPause() {
 *         super.onPause();
 *         if (session != null) session.pause();
 *     }
 *
 *     @Override
 *     protected void onResume() {
 *         super.onResume();
 *         if (session != null) session.resume();
 *     }
 *
 *     @Override
 *     protected void onDestroy() {
 *         if (session != null) {
 *             session.close();
 *             session = null;
 *         }
 *         super.onDestroy();
 *     }
 * }
 * }</pre>
 *
 * <h3>Thread Safety</h3>
 * <p>
 * This class is thread-safe except for the Activity-bound session creation
 * overload, which must run on the main thread because it configures Android UI
 * state. {@link #createSessionSafe(Activity, Surface, RuntimeConfig, String)}
 * reports a caller-thread violation as a failure result instead of throwing it.
 *
 */
public final class MigoRuntime {

    /**
     * Java SDK version, from the same {@code release/VERSION} the AAR's
     * {@code versionName} and the native library's {@code CARGO_PKG_VERSION}
     * derive from. Held here as an alias of {@link BuildInfo#VERSION} rather
     * than a literal: a hardcoded value drifted to an early release's number
     * while the rest of the build moved on, and the skew check below then
     * compared two frozen strings and never fired.
     */
    public static final String SDK_VERSION = BuildInfo.VERSION;
    /** Fallback API floor used when native metadata is unavailable. */
    private static final int FALLBACK_MIN_SDK = 26;

    private static final Object LOCK = new Object();
    private static volatile MigoRuntime sInstance;

    private final Object nativeLock = new Object();
    private volatile boolean nativeLoaded = false;
    private volatile String nativeVersion = "unknown";
    private volatile Throwable nativeLoadError;

    /**
     * Get the singleton instance of MigoRuntime.
     *
     * @return The MigoRuntime instance
     */
    public static MigoRuntime getInstance() {
        if (sInstance == null) {
            synchronized (LOCK) {
                if (sInstance == null) {
                    sInstance = new MigoRuntime();
                }
            }
        }
        return sInstance;
    }

    private MigoRuntime() {
        // Deliberately empty. Obtaining the singleton must not pull ~45 MB of
        // engine into the process: a host that delivers the binary itself
        // (see MigoNativeLoader) needs to wire Migo up in Application.onCreate
        // and only pay for the engine when a user actually opens a game. Every
        // accessor below that needs native calls ensureNative() first, so the
        // packaged default behaves exactly as it did when this constructor
        // loaded the library.
    }

    /**
     * Load the engine on first use.
     *
     * @return true when native calls may be made
     */
    private boolean ensureNative() {
        if (nativeLoaded) {
            return true;
        }
        synchronized (nativeLock) {
            if (nativeLoaded) {
                return true;
            }
            if (!MigoNativeLoader.ensureLoaded()) {
                nativeVersion = "load_failed";
                nativeLoadError = MigoNativeLoader.lastError();
                return false;
            }
            String reported;
            try {
                reported = NativeMethods.getLoadedVersion();
            } catch (UnsatisfiedLinkError e) {
                // The library is in the process but its JNI registration is
                // not what this SDK expects -- a mismatched engine that
                // dlopen() had no reason to reject.
                nativeVersion = "load_failed";
                nativeLoadError = e;
                Log.e("MigoRuntime", "Native library loaded but unusable: " + e.getMessage());
                return false;
            }
            nativeVersion = reported == null ? "unknown" : reported;
            if (!nativeVersion.equals(SDK_VERSION)) {
                // Both sides now derive from release/VERSION -- the AAR through
                // BuildInfo.VERSION, the .so through CARGO_PKG_VERSION -- so this
                // fires only when an AAR is paired with a native library built
                // from a different tree. A host-delivered binary cannot reach
                // here mismatched: it is checked against this build's artifact
                // manifest before load. This stays a warning for the packaged
                // case, where the AAR's own contents are gated at build time.
                Log.w("MigoRuntime", "SDK/native version mismatch: SDK=" + SDK_VERSION
                        + " native=" + nativeVersion);
            }
            nativeLoadError = null;
            nativeLoaded = true;
            return true;
        }
    }

    // ==================== Version Info ====================

    /**
     * Get the SDK version string, from {@code release/VERSION} via
     * {@link BuildInfo#VERSION}.
     *
     * @return the SDK version, the same value as {@link #SDK_VERSION}
     */
    public String getVersion() {
        return BuildInfo.VERSION;
    }

    /**
     * Get the native engine version string, reported by the loaded library
     * from its {@code CARGO_PKG_VERSION}. Equal to {@link #getVersion()} for a
     * consistently built pair.
     *
     * @return the native engine version, or "unknown" / "load_failed" if the
     *         library is not usable
     */
    public String getNativeVersion() {
        ensureNative();
        return nativeVersion;
    }

    /**
     * Check if the native library is loaded.
     *
     * @return true if native library loaded successfully
     */
    public boolean isNativeLoaded() {
        return ensureNative();
    }

    /**
     * Returns the error that prevented native library loading, or null if loaded successfully.
     * Useful for diagnosing integration issues (wrong ABI, missing .so, etc.)
     *
     * @return The load error, or null if the native library loaded successfully
     */
    public Throwable getNativeLoadError() {
        ensureNative();
        return nativeLoadError;
    }

    /**
     * Get the minimum supported Android API level.
     *
     * @return Minimum API level required by the native engine
     */
    public int getMinSdkVersion() {
        if (!ensureNative()) {
            return FALLBACK_MIN_SDK;
        }
        try {
            return NativeMethods.getMinApiLevel();
        } catch (Throwable t) {
            return FALLBACK_MIN_SDK;
        }
    }

    /**
     * Initialize ICU data for text shaping when the runtime was built
     * with {@code external_icudtl}.
     * <p>
     * In the default embedded-ICU build this returns {@code true}
     * immediately. In external-ICU builds, call this once before
     * creating any {@link GameSession}.
     *
     * @param icuDataPath absolute path to {@code icudtl.dat}
     * @return {@code true} if ICU data is ready for text layout
     */
    public boolean initIcuData(String icuDataPath) {
        if (!ensureNative()) {
            return false;
        }
        if (icuDataPath == null || icuDataPath.trim().isEmpty()) {
            throw new IllegalArgumentException("icuDataPath cannot be null or empty");
        }
        return NativeMethods.initIcuData(icuDataPath);
    }

    // ==================== Session Creation ====================

    /**
     * Create a new game session with isolated file system.
     * <p>
     * This is the recommended way to create a session. It automatically sets up
     * full-screen immersive mode, initializes the application context, and creates
     * isolated directories for the game.
     *
     * @param activity The host Activity
     * @param surface  The rendering Surface
     * @param config   Runtime configuration
     * @param gameId   Unique game identifier (alphanumeric, underscore, hyphen, 1-64 chars)
     * @return A new GameSession
     * @throws MigoException if creation fails or gameId is invalid
     * @throws IllegalStateException if called off the main thread
     */
    public GameSession createSession(Activity activity, Surface surface, RuntimeConfig config, String gameId) {
        ThreadCheck.ensureMainThread();
        validateCreateSession(activity, surface, config);
        validateGameId(gameId);

        // The engine may not be in the process yet: it is loaded on first use,
        // and a host that delivers the binary itself may not have delivered it.
        // Without this the failure surfaces as an UnsatisfiedLinkError out of
        // the JNI call below, naming a method rather than the real cause.
        if (!ensureNative()) {
            throw new MigoException(ErrorCode.ERR_NATIVE_LOAD_FAILED,
                    "Native library not loaded", nativeLoadError);
        }

        // Initialize app context
        AppContext.init(activity);

        // Enter full-screen immersive mode (if enabled)
        if (config.isImmersiveMode()) {
            enterImmersiveMode(activity);
        }

        // Create native session
        int sessionId = NativeMethods.init(surface, config);
        if (sessionId < 0) {
            throw new MigoException(ErrorCode.ERR_INIT_FAILED,
                    ErrorCode.getMessage(ErrorCode.ERR_INIT_FAILED) + ": Native init returned " + sessionId);
        }

        // Create Java session wrapper with gameId
        GameSession session = new GameSession(sessionId, gameId, config, activity, true);

        // Register runtime context
        RuntimeRegistry.register(new RuntimeContext(sessionId, activity));

        return session;
    }

    /**
     * Create a session before its rendering Surface exists.
     * <p>
     * Everything the engine can do without a window happens immediately and in
     * parallel with the host's own startup: the host thread and its V8 isolate,
     * and on the render thread the EGL display and config, the resource
     * context, the GLES dispatch table, capability detection, Skia and the
     * system font scan. The session then waits. Call
     * {@link GameSession#updateSurface(Surface, int, int)} when the Surface
     * arrives, and {@link GameSession#startGame} after that.
     * <p>
     * The gain is not that any of that work got cheaper -- it is that an
     * Activity spends real time between {@code onCreate} and
     * {@code surfaceCreated} (~150 ms on a Mate 30 Pro, more when it rotates
     * for a landscape game) during which the engine used to be doing nothing at
     * all, because it had not been allowed to start yet.
     * <p>
     * A game must not be started before a Surface is attached: module
     * evaluation waits on GPU capabilities, and the first thing an adapted
     * browser game asks for is the window size.
     *
     * @param activity The host Activity
     * @param config   Runtime configuration
     * @param gameId   Unique game identifier (alphanumeric, underscore, hyphen, 1-64 chars)
     * @return A new GameSession with no Surface attached yet
     * @throws MigoException if creation fails or gameId is invalid
     */
    public GameSession createSessionWarm(Activity activity, RuntimeConfig config, String gameId) {
        ThreadCheck.ensureMainThread();
        validateCreateSession(activity, null, config, false);
        validateGameId(gameId);

        if (!ensureNative()) {
            throw new MigoException(ErrorCode.ERR_NATIVE_LOAD_FAILED,
                    "Native library not loaded", nativeLoadError);
        }

        AppContext.init(activity);

        if (config.isImmersiveMode()) {
            enterImmersiveMode(activity);
        }

        // The warm start is a null Surface on the native side too; the engine
        // brings up everything a window is not needed for and then parks.
        int sessionId = NativeMethods.initWarm(config);
        if (sessionId < 0) {
            throw new MigoException(ErrorCode.ERR_INIT_FAILED,
                    ErrorCode.getMessage(ErrorCode.ERR_INIT_FAILED) + ": Native init returned " + sessionId);
        }

        GameSession session = new GameSession(sessionId, gameId, config, activity, false);
        RuntimeRegistry.register(new RuntimeContext(sessionId, activity));
        return session;
    }

    /**
     * Create a new game session without Activity binding.
     * <p>
     * Use this method when creating a session without an Activity, such as in a Service
     * or when using a standalone SurfaceView. Note that some features requiring Activity
     * (like permissions, system settings) will not work.
     *
     * @param context Application or Activity context
     * @param surface The rendering Surface
     * @param config  Runtime configuration
     * @param gameId  Unique game identifier (alphanumeric, underscore, hyphen, 1-64 chars)
     * @return A new GameSession
     * @throws MigoException if creation fails or gameId is invalid
     */
    public GameSession createSession(Context context, Surface surface, RuntimeConfig config, String gameId) {
        if (context == null) {
            throw new MigoException(ErrorCode.ERR_INVALID_CONFIG,
                    ErrorCode.getMessage(ErrorCode.ERR_INVALID_CONFIG) + ": context is null");
        }
        if (surface == null) {
            throw new MigoException(ErrorCode.ERR_INVALID_SURFACE,
                    ErrorCode.getMessage(ErrorCode.ERR_INVALID_SURFACE));
        }
        if (config == null) {
            throw new MigoException(ErrorCode.ERR_INVALID_CONFIG,
                    ErrorCode.getMessage(ErrorCode.ERR_INVALID_CONFIG));
        }
        validateGameId(gameId);

        if (!ensureNative()) {
            throw new MigoException(ErrorCode.ERR_NATIVE_LOAD_FAILED,
                    "Native library not loaded", nativeLoadError);
        }

        // Initialize app context
        AppContext.init(context);

        // Create native session
        int sessionId = NativeMethods.init(surface, config);
        if (sessionId < 0) {
            throw new MigoException(ErrorCode.ERR_INIT_FAILED,
                    ErrorCode.getMessage(ErrorCode.ERR_INIT_FAILED) + ": Native init returned " + sessionId);
        }

        // Create Java session wrapper with gameId
        GameSession session = new GameSession(sessionId, gameId, config, context, true);

        // Register without Activity
        RuntimeRegistry.register(new RuntimeContext(sessionId, null));

        return session;
    }

    /**
     * Create a new game session with isolated file system.
     * <p>
     * Non-throwing version that returns a result wrapper, including when it is
     * called off the Android main thread.
     *
     * @param activity The host Activity
     * @param surface  The rendering Surface
     * @param config   Runtime configuration
     * @param gameId   Unique game identifier
     * @return Result containing either the session or an error code
     */
    public Result<GameSession> createSessionSafe(Activity activity, Surface surface, RuntimeConfig config, String gameId) {
        try {
            ThreadCheck.ensureMainThread();
            return Result.success(createSession(activity, surface, config, gameId));
        } catch (MigoException e) {
            return Result.failure(e.getErrorCode(), e.getMessage());
        } catch (Exception e) {
            // Framework exceptions may embed app-private filesystem paths or
            // Activity implementation details. The non-throwing public API
            // exposes a stable code/message pair, while callers that need the
            // original failure can use the throwing overload under a debugger.
            return Result.failure(
                    ErrorCode.ERR_INIT_FAILED,
                    ErrorCode.getMessage(ErrorCode.ERR_INIT_FAILED));
        }
    }

    /**
     * Create a session before its Surface exists.
     * <p>
     * Non-throwing version of {@link #createSessionWarm}.
     *
     * @param activity The host Activity
     * @param config   Runtime configuration
     * @param gameId   Unique game identifier
     * @return Result containing either the session or an error code
     */
    public Result<GameSession> createSessionWarmSafe(Activity activity, RuntimeConfig config, String gameId) {
        ThreadCheck.ensureMainThread();
        try {
            return Result.success(createSessionWarm(activity, config, gameId));
        } catch (MigoException e) {
            return Result.failure(e.getErrorCode(), e.getMessage());
        } catch (Exception e) {
            return Result.failure(ErrorCode.ERR_INIT_FAILED, e.getMessage());
        }
    }

    // ==================== Utility Methods ====================

    /**
     * Check if the current device meets minimum requirements.
     * <p>
     * The minimum API level is sourced from the native engine
     * (queried through the internal checked bridge), so this method
     * stays in sync with the build script's {@code ANDROID_API}
     * without requiring a Java-side constant update on every
     * NDK preset change.  Returns {@code false} if the native
     * library failed to load.
     *
     * @return true if device is supported
     */
    public boolean isDeviceSupported() {
        if (!ensureNative()) {
            return false;
        }
        return Build.VERSION.SDK_INT >= getMinSdkVersion();
    }

    /**
     * Get the number of active sessions.
     *
     * @return Number of active sessions
     */
    public int getActiveSessionCount() {
        return RuntimeRegistry.size();
    }

    // ==================== Private Methods ====================

    private void validateCreateSession(Activity activity, Surface surface, RuntimeConfig config) {
        validateCreateSession(activity, surface, config, true);
    }

    private void validateCreateSession(Activity activity, Surface surface, RuntimeConfig config,
                                       boolean surfaceRequired) {
        if (activity == null || activity.isFinishing() || activity.isDestroyed()) {
            throw new MigoException(ErrorCode.ERR_INVALID_ACTIVITY,
                    ErrorCode.getMessage(ErrorCode.ERR_INVALID_ACTIVITY));
        }
        if (surfaceRequired && surface == null) {
            throw new MigoException(ErrorCode.ERR_INVALID_SURFACE,
                    ErrorCode.getMessage(ErrorCode.ERR_INVALID_SURFACE));
        }
        if (config == null) {
            throw new MigoException(ErrorCode.ERR_INVALID_CONFIG,
                    ErrorCode.getMessage(ErrorCode.ERR_INVALID_CONFIG));
        }
        if (!ensureNative()) {
            throw new MigoException(ErrorCode.ERR_NATIVE_LOAD_FAILED,
                    "Native library not loaded", nativeLoadError);
        }
    }

    private void validateGameId(String gameId) {
        if (!GamePaths.isValidGameId(gameId)) {
            throw new MigoException(ErrorCode.ERR_INVALID_CONFIG,
                    ErrorCode.getMessage(ErrorCode.ERR_INVALID_CONFIG) + ": Invalid gameId: must be 1-64 lower-case alphanumeric characters, underscore or hyphen, and not a reserved device name");
        }
    }

    private void enterImmersiveMode(Activity activity) {
        // Use DisplayCompat for API 26+ compatibility
        DisplayCompat.enterImmersiveMode(activity);
    }

    // ==================== Inner Classes ====================

    /**
     * Result wrapper for non-throwing operations.
     *
     * @param <T> The success value type
     */
    public static final class Result<T> {
        private final T value;
        private final int errorCode;
        private final String errorMessage;

        private Result(T value, int errorCode, String errorMessage) {
            this.value = value;
            this.errorCode = errorCode;
            this.errorMessage = errorMessage;
        }

        static <T> Result<T> success(T value) {
            return new Result<>(value, ErrorCode.SUCCESS, null);
        }

        static <T> Result<T> failure(int errorCode, String message) {
            return new Result<>(null, errorCode, message);
        }

        /** Check if operation succeeded */
        public boolean isSuccess() {
            return errorCode == ErrorCode.SUCCESS;
        }

        /** Check if operation failed */
        public boolean isFailure() {
            return errorCode != ErrorCode.SUCCESS;
        }

        /** Get the success value (null if failed) */
        public T getValue() {
            return value;
        }

        /** Get the error code */
        public int getErrorCode() {
            return errorCode;
        }

        /** Get the error message (null if succeeded) */
        public String getErrorMessage() {
            return errorMessage;
        }

        /** Get value or throw if failed */
        public T getOrThrow() {
            if (isFailure()) {
                throw new MigoException(errorCode, errorMessage);
            }
            return value;
        }
    }
}
