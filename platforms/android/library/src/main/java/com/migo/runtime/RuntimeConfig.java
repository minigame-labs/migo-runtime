package com.migo.runtime;

import android.content.Context;

import org.json.JSONObject;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/**
 * Configuration for initializing the Migo Runtime.
 * <p>
 * Use the {@link Builder} to create instances:
 *
 * <pre>{@code
 * RuntimeConfig config = new RuntimeConfig.Builder(context)
 *     .setTargetFps(60)
 *     .setDebugEnabled(BuildConfig.DEBUG)
 *     .setLogLevel(RuntimeConfig.LogLevel.DEBUG)
 *     .build();
 * }</pre>
 *
 */
public final class RuntimeConfig {

    /**
     * Log level for the native engine.
     */
    public enum LogLevel {
        /** Log everything including trace messages */
        TRACE(0),
        /** Log debug and above */
        DEBUG(1),
        /** Log info and above */
        INFO(2),
        /** Log warnings and above (default) */
        WARN(3),
        /** Log errors only */
        ERROR(4),
        /** Disable logging */
        OFF(5);

        private final int value;

        LogLevel(int value) {
            this.value = value;
        }

        /**
         * Get the integer value for JNI.
         * @return Integer value
         */
        public int getValue() {
            return value;
        }
    }

    /**
     * Which build of the content a session runs: what {@code getAccountInfoSync}
     * reports, as the constant's name in lower case.
     */
    public enum ContentEnvironment {
        /** The released build -- the default. */
        RELEASE,
        /** A pre-release build handed to testers. */
        TRIAL,
        /** A development build. */
        DEVELOP
    }

    /** The longest content version the engine keeps, in UTF-8 bytes. */
    public static final int CONTENT_VERSION_MAX_BYTES = 64;

    // Core settings
    private final String cacheDir;
    private final String filesDir;
    private final String codeCacheDir;
    private final float displayDensity;

    // Performance settings
    private final int targetFps;

    // Debug settings
    private final boolean debugEnabled;
    private final LogLevel logLevel;

    // Display settings
    private final boolean immersiveMode;
    private final String startupOrientation;

    // Safety settings
    private final boolean watchdogEnabled;
    private final int watchdogTimeoutSecs;
    private final boolean codeSigningEnabled;
    private final String codeSigningPubkey;

    // What the host says about the content and the device
    private final ContentEnvironment contentEnvironment;
    private final String contentVersion;
    private final int benchmarkLevel;
    private final int modelLevel;

    // Boot prelude scripts: pairs of {name, source} executed before every
    // EvaluateModule. Used to inject a BOM/DOM adapter so browser-style
    // games can run unchanged. See Builder#addPreludeScript.
    private final List<String[]> preludeScripts;
    // Internal: serialized preludeScripts for JNI field access
    private final String preludeScriptsJson;
    // Internal: the NetworkPolicy, in the form the engine parses, for JNI field access
    private final String networkPolicyJson;
    // Internal: what the game is launched with, as JSON, or null for none -- for JNI field access
    private final String launchOptionsJson;

    private RuntimeConfig(Builder builder) {
        this.cacheDir = builder.cacheDir;
        this.filesDir = builder.filesDir;
        this.codeCacheDir = builder.codeCacheDir;
        this.displayDensity = builder.displayDensity;
        this.targetFps = builder.targetFps;
        this.debugEnabled = builder.debugEnabled;
        this.logLevel = builder.logLevel;
        this.immersiveMode = builder.immersiveMode;
        this.startupOrientation = builder.startupOrientation;
        this.watchdogEnabled = builder.watchdogEnabled;
        this.watchdogTimeoutSecs = builder.watchdogTimeoutSecs;
        this.codeSigningEnabled = builder.codeSigningEnabled;
        this.codeSigningPubkey = builder.codeSigningPubkey;
        this.contentEnvironment = builder.contentEnvironment;
        this.contentVersion = builder.contentVersion;
        this.benchmarkLevel = builder.benchmarkLevel;
        this.modelLevel = builder.modelLevel;
        this.preludeScripts = Collections.unmodifiableList(new ArrayList<>(builder.preludeScripts));
        this.preludeScriptsJson = buildPreludeScriptsJson(this.preludeScripts);
        this.networkPolicyJson = builder.networkPolicy.toJson();
        this.launchOptionsJson = builder.launchOptionsJson;
    }

    // ==================== Getters ====================

    /** Get the cache directory path (may be cleared by system) */
    public String getCacheDir() { return cacheDir; }

    /** Get the files directory path (persistent storage) */
    public String getFilesDir() { return filesDir; }

    /** Get the code cache directory path */
    public String getCodeCacheDir() { return codeCacheDir; }

    /** Get the display density (pixels per dp) */
    public float getDisplayDensity() { return displayDensity; }

    /** Get the target frames per second */
    public int getTargetFps() { return targetFps; }

    /** Check if debug mode is enabled */
    public boolean isDebugEnabled() { return debugEnabled; }

    /** Get the log level */
    public LogLevel getLogLevel() { return logLevel; }

    /** Check whether full-screen immersive mode is enabled */
    public boolean isImmersiveMode() { return immersiveMode; }

    /**
     * Get preferred startup orientation.
     *
     * @return "landscape", "portrait", or null
     */
    public String getStartupOrientation() { return startupOrientation; }

    /** Check whether ANR watchdog is enabled */
    public boolean isWatchdogEnabled() { return watchdogEnabled; }

    /** Get ANR watchdog timeout in seconds */
    public int getWatchdogTimeoutSecs() { return watchdogTimeoutSecs; }

    /** Check whether code signing verification is enabled */
    public boolean isCodeSigningEnabled() { return codeSigningEnabled; }

    /** Get hex Ed25519 public key used by code signing verification (nullable) */
    public String getCodeSigningPubkey() { return codeSigningPubkey; }

    /** Which build of the content the session runs. */
    public ContentEnvironment getContentEnvironment() { return contentEnvironment; }

    /** The content's released version, or null for none. */
    public String getContentVersion() { return contentVersion; }

    /** The app's own score for the device: -1 unknown, 1 and up higher faster. */
    public int getBenchmarkLevel() { return benchmarkLevel; }

    /** The app's own tier for the device: 0 unknown, 1 high, 2 middle, 3 low. */
    public int getModelLevel() { return modelLevel; }

    /**
     * Get the configured boot prelude scripts.
     * Each entry is a {@code String[2]} of {@code {name, source}} run in
     * declaration order before every {@code EvaluateModule}.
     *
     * @return Unmodifiable list of prelude scripts (may be empty)
     */
    public List<String[]> getPreludeScripts() { return preludeScripts; }

    // ==================== JNI field access ====================

    /** @hide */
    int getLogLevelOrdinal() { return logLevel.ordinal(); }

    @Override
    public String toString() {
        return "RuntimeConfig{" +
                "privateDirectories=redacted" +
                ", targetFps=" + targetFps +
                ", debugEnabled=" + debugEnabled +
                ", logLevel=" + logLevel +
                ", startupOrientation='" + startupOrientation + '\'' +
                ", watchdogEnabled=" + watchdogEnabled +
                ", watchdogTimeoutSecs=" + watchdogTimeoutSecs +
                ", codeSigningEnabled=" + codeSigningEnabled +
                ", networkPolicy=" + networkPolicyJson +
                '}';
    }

    /**
     * Serialize prelude scripts to JSON for JNI transfer.
     * Returns null if the list is empty. Each entry is a
     * {@code {"name":"...","source":"..."}} object with the same key
     * shape the Rust side parses on `preludeScriptsJson`.
     */
    private static String buildPreludeScriptsJson(List<String[]> list) {
        if (list == null || list.isEmpty()) return null;
        StringBuilder sb = new StringBuilder("[");
        for (int i = 0; i < list.size(); i++) {
            if (i > 0) sb.append(",");
            sb.append("{\"name\":\"");
            sb.append(escapeJsonString(list.get(i)[0]));
            sb.append("\",\"source\":\"");
            sb.append(escapeJsonString(list.get(i)[1]));
            sb.append("\"}");
        }
        sb.append("]");
        return sb.toString();
    }

    private static String escapeJsonString(String s) {
        // Cover the JSON control set; prelude sources can contain anything.
        StringBuilder out = new StringBuilder(s.length() + 8);
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            switch (c) {
                case '\\': out.append("\\\\"); break;
                case '"':  out.append("\\\""); break;
                case '\n': out.append("\\n"); break;
                case '\r': out.append("\\r"); break;
                case '\t': out.append("\\t"); break;
                case '\b': out.append("\\b"); break;
                case '\f': out.append("\\f"); break;
                default:
                    if (c < 0x20) {
                        out.append(String.format("\\u%04x", (int) c));
                    } else {
                        out.append(c);
                    }
            }
        }
        return out.toString();
    }

    // ==================== Builder ====================

    /**
     * Builder for creating RuntimeConfig instances.
     */
    public static final class Builder {
        // Core (auto-detected from context)
        private String cacheDir;
        private String filesDir;
        private String codeCacheDir;
        private float displayDensity = 1.0f;

        // Performance
        private int targetFps = 60;

        // Debug
        private boolean debugEnabled = false;
        private LogLevel logLevel = LogLevel.WARN;

        // Display
        private boolean immersiveMode = true;
        private String startupOrientation = null;

        // Safety
        private boolean watchdogEnabled = true;
        private int watchdogTimeoutSecs = 10;
        private boolean codeSigningEnabled = true;
        private String codeSigningPubkey = null;

        // Content and device
        private ContentEnvironment contentEnvironment = ContentEnvironment.RELEASE;
        private String contentVersion = null;
        private int benchmarkLevel = -1;
        private int modelLevel = 0;

        private final List<String[]> preludeScripts = new ArrayList<>();
        private NetworkPolicy networkPolicy = NetworkPolicy.denyAll();
        private String launchOptionsJson = null;

        /**
         * Create a new builder with required context.
         * <p>
         * Automatically extracts directories and display density from context.
         *
         * @param context Android context (Activity or Application)
         */
        public Builder(Context context) {
            if (context == null) {
                throw new IllegalArgumentException("Context cannot be null");
            }
            this.cacheDir = context.getCacheDir().getAbsolutePath();
            this.filesDir = context.getFilesDir().getAbsolutePath();
            this.codeCacheDir = this.cacheDir;
            this.displayDensity = context.getResources().getDisplayMetrics().density;
        }

        /**
         * Create a new builder with manual configuration.
         * <p>
         * Use this constructor when context is not available.
         *
         * @param cacheDir       Cache directory path
         * @param filesDir       Files directory path (persistent)
         * @param displayDensity Display density (dp ratio)
         */
        public Builder(String cacheDir, String filesDir, float displayDensity) {
            if (cacheDir == null || cacheDir.isEmpty()) {
                throw new IllegalArgumentException("cacheDir cannot be null or empty");
            }
            if (filesDir == null || filesDir.isEmpty()) {
                throw new IllegalArgumentException("filesDir cannot be null or empty");
            }
            this.cacheDir = cacheDir;
            this.filesDir = filesDir;
            this.codeCacheDir = cacheDir;
            this.displayDensity = displayDensity > 0 ? displayDensity : 1.0f;
        }

        /**
         * Set the code cache directory.
         * <p>
         * Used for storing compiled code. Defaults to the main cache directory.
         *
         * @param dir Absolute path to code cache directory
         * @return this builder
         */
        public Builder setCodeCacheDir(String dir) {
            if (dir != null && !dir.isEmpty()) {
                this.codeCacheDir = dir;
            }
            return this;
        }

        /**
         * Set the target frame rate.
         * <p>
         * Default: 60 FPS. The engine serves 1-240 and is the single authority on
         * that range, so the value is carried through as given rather than clamped
         * again here: a second copy of the rule is how a builder ends up silently
         * raising a 24 FPS request to 30, or capping a 144 Hz panel at 120.
         *
         * @param fps Target frames per second
         * @return this builder
         */
        public Builder setTargetFps(int fps) {
            this.targetFps = fps;
            return this;
        }

        /**
         * Set preferred startup orientation.
         *
         * @param orientation "landscape", "portrait", or null
         * @return this builder
         */
        public Builder setStartupOrientation(String orientation) {
            if (orientation == null || orientation.isEmpty()) {
                this.startupOrientation = null;
            } else if ("landscape".equals(orientation) || "portrait".equals(orientation)) {
                this.startupOrientation = orientation;
            } else {
                throw new IllegalArgumentException(
                        "startupOrientation must be \"landscape\" or \"portrait\"");
            }
            return this;
        }

        /**
         * Which build of the content the session runs: what
         * {@code getAccountInfoSync().miniProgram.envVersion} reports.
         * <p>
         * Default: {@link ContentEnvironment#RELEASE}.
         *
         * @param environment the build
         * @return this builder
         */
        public Builder setContentEnvironment(ContentEnvironment environment) {
            if (environment == null) {
                throw new IllegalArgumentException("contentEnvironment must not be null");
            }
            this.contentEnvironment = environment;
            return this;
        }

        /**
         * The content's released version, what
         * {@code getAccountInfoSync().miniProgram.version} reports for a release.
         * A build that is not a release reports none, as the platform does.
         * <p>
         * Default: none.
         *
         * @param version the version, or null or empty for none; at most
         *                {@link #CONTENT_VERSION_MAX_BYTES} UTF-8 bytes
         * @return this builder
         */
        public Builder setContentVersion(String version) {
            if (version == null || version.isEmpty()) {
                this.contentVersion = null;
                return this;
            }
            if (version.getBytes(java.nio.charset.StandardCharsets.UTF_8).length
                    > CONTENT_VERSION_MAX_BYTES) {
                throw new IllegalArgumentException(
                        "contentVersion is longer than " + CONTENT_VERSION_MAX_BYTES + " bytes");
            }
            this.contentVersion = version;
            return this;
        }

        /**
         * The app's own assessment of the device, what
         * {@code getDeviceBenchmarkInfo} and {@code getDeviceInfo().benchmarkLevel}
         * report. The engine measures neither: without this they report unknown.
         * <p>
         * Default: -1 and 0, unknown.
         *
         * @param benchmarkLevel -1 unknown, 1 and up higher faster (-2 and 0: the
         *                       device cannot run games)
         * @param modelLevel     0 unknown, 1 high, 2 middle, 3 low
         * @return this builder
         */
        public Builder setDeviceBenchmark(int benchmarkLevel, int modelLevel) {
            if (benchmarkLevel < -2) {
                throw new IllegalArgumentException("benchmarkLevel must be -2 or more");
            }
            if (modelLevel < 0 || modelLevel > 3) {
                throw new IllegalArgumentException("modelLevel must be 0 to 3");
            }
            this.benchmarkLevel = benchmarkLevel;
            this.modelLevel = modelLevel;
            return this;
        }

        /**
         * Enable or disable debug mode.
         * <p>
         * Default: false. Enable for development, disable for production.
         *
         * @param enabled true to enable debug features
         * @return this builder
         */
        public Builder setDebugEnabled(boolean enabled) {
            this.debugEnabled = enabled;
            return this;
        }

        /**
         * Set the log level for native code.
         * <p>
         * Default: WARN. Use DEBUG or TRACE for development.
         *
         * @param level Log level
         * @return this builder
         */
        public Builder setLogLevel(LogLevel level) {
            this.logLevel = level != null ? level : LogLevel.WARN;
            return this;
        }

        /**
         * Enable or disable full-screen immersive mode.
         * <p>
         * Default: true. Set to false when embedding the game view
         * alongside other UI elements (e.g., in a feed or partial-screen layout).
         *
         * @param enabled true to enter immersive mode on session creation
         * @return this builder
         */
        public Builder setImmersiveMode(boolean enabled) {
            this.immersiveMode = enabled;
            return this;
        }

        /**
         * Enable or disable the native watchdog.
         * <p>
         * Default: enabled.
         */
        public Builder setWatchdogEnabled(boolean enabled) {
            this.watchdogEnabled = enabled;
            return this;
        }

        /**
         * Set watchdog timeout in seconds.
         * <p>
         * Values are clamped to [5, 120]. Default: 10.
         */
        public Builder setWatchdogTimeoutSecs(int timeoutSecs) {
            this.watchdogTimeoutSecs = Math.max(5, Math.min(120, timeoutSecs));
            return this;
        }

        /**
         * Enable or disable code signing verification.
         * <p>
         * Default: enabled.
         */
        public Builder setCodeSigningEnabled(boolean enabled) {
            this.codeSigningEnabled = enabled;
            return this;
        }

        /**
         * Set Ed25519 public key (hex, 64 chars) for code signing verification.
         * <p>
         * Required when code signing is enabled.
         */
        public Builder setCodeSigningPubkey(String pubkeyHex) {
            if (pubkeyHex == null) {
                this.codeSigningPubkey = null;
            } else {
                String trimmed = pubkeyHex.trim();
                this.codeSigningPubkey = trimmed.isEmpty() ? null : trimmed;
            }
            return this;
        }

        /**
         * Add a boot prelude script.
         * <p>
         * The script runs as a plain (non-module) snippet in the global
         * scope before every {@code EvaluateModule}. Use this to inject
         * a BOM/DOM adapter (e.g. the {@code migo-adapter} IIFE bundle)
         * so browser-style games (Cocos / Egret / Laya / Pixi / raw WebGL)
         * load unchanged on top of the {@code migo.*} runtime.
         * <p>
         * Multiple calls accumulate; scripts execute in declaration order,
         * and each call is independent of the others. {@code name} appears
         * in V8 stack traces.
         *
         * @param name   Script name shown in stack traces (e.g. {@code "<migo-adapter>"})
         * @param source JavaScript source — NOT an ES module
         * @return this builder
         * @throws IllegalArgumentException if {@code name} or {@code source} is null/empty
         */
        public Builder addPreludeScript(String name, String source) {
            if (name == null || name.trim().isEmpty()) {
                throw new IllegalArgumentException("prelude script name cannot be null or empty");
            }
            if (source == null || source.isEmpty()) {
                throw new IllegalArgumentException("prelude script source cannot be null or empty");
            }
            this.preludeScripts.add(new String[]{name.trim(), source});
            return this;
        }

        /**
         * Set what network the game may reach.
         * <p>
         * Default: {@link NetworkPolicy#denyAll()} -- a game given no policy reaches
         * no network at all.
         *
         * @throws IllegalArgumentException if {@code policy} is null
         */
        public Builder setNetworkPolicy(NetworkPolicy policy) {
            if (policy == null) {
                throw new IllegalArgumentException(
                        "networkPolicy must not be null; use NetworkPolicy.denyAll() for none");
            }
            this.networkPolicy = policy;
            return this;
        }

        /**
         * Set what the game is launched with: what {@code getLaunchOptionsSync()} answers -- the scene, the query of
         * the link that opened it, who referred it ({@code scene}, {@code query}, {@code shareTicket},
         * {@code apiCategory}, {@code referrerInfo}). What it leaves out has its default, and so does a field of the
         * wrong type. It is also what the game is entered with until a show carries options of its own, and what a
         * restart launches it with again, its query the one {@code restartMiniProgram}'s path names.
         * <p>
         * Default: none -- scene 1001 and an empty query, unless the activity's intent carries launch options.
         *
         * @param options the options, copied as they are now; null for none
         */
        public Builder setLaunchOptions(JSONObject options) {
            this.launchOptionsJson = options == null ? null : options.toString();
            return this;
        }

        /**
         * Build the RuntimeConfig instance.
         *
         * @return Configured RuntimeConfig
         * @throws IllegalStateException if required fields are not set
         */
        public RuntimeConfig build() {
            if (cacheDir == null || cacheDir.isEmpty()) {
                throw new IllegalStateException("cacheDir is required");
            }
            return new RuntimeConfig(this);
        }
    }
}
