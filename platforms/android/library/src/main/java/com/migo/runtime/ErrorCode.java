package com.migo.runtime;

/**
 * Error codes for the Migo Runtime SDK.
 * <p>
 * SDK and platform errors are negative; native engine errors are positive.
 * Success is indicated by code 0.
 *
 */
public final class ErrorCode {

    private ErrorCode() {}

    // ==================== Success ====================
    /** Operation completed successfully */
    public static final int SUCCESS = 0;

    // ==================== Initialization Errors (-1xxx) ====================
    /** Generic initialization failure */
    public static final int ERR_INIT_FAILED = -1000;
    /** Surface is null or invalid */
    public static final int ERR_INVALID_SURFACE = -1001;
    /** Configuration is null or invalid */
    public static final int ERR_INVALID_CONFIG = -1002;
    /** Native library failed to load */
    public static final int ERR_NATIVE_LOAD_FAILED = -1003;
    /** Game ID or entry point is missing or invalid */
    public static final int ERR_INVALID_GAME_ID = -1004;
    /** Device does not meet the requirements to run the engine */
    public static final int ERR_NOT_SUPPORTED = -1005;

    // ==================== Runtime Errors (-2xxx) ====================
    /** Session has been destroyed */
    public static final int ERR_SESSION_DESTROYED = -2000;
    /** Game code directory not found */
    public static final int ERR_CODE_DIR_NOT_FOUND = -2002;
    /** Entry point file not found */
    public static final int ERR_ENTRY_NOT_FOUND = -2003;
    /** JavaScript execution error */
    public static final int ERR_JS_EXECUTION = -2004;
    /** Terminal platform resource cleanup did not fully complete */
    public static final int ERR_CLEANUP_FAILED = -2005;

    // ==================== Platform Errors (-5xxx) ====================
    /** Activity is null or finishing */
    public static final int ERR_INVALID_ACTIVITY = -5004;

    // ==================== Native Engine Errors (positive, from Rust ErrorCode) ====================
    /**
     * V8 heap out-of-memory: the near-heap-limit callback fired and the
     * JS isolate was terminated to prevent a crash.
     */
    public static final int NATIVE_OUT_OF_MEMORY = 203;

    /**
     * JS execution timeout: a long-running script exceeded the watchdog
     * timeout and was forcefully terminated.
     */
    public static final int NATIVE_JS_EXECUTION_TIMEOUT = 204;

    /**
     * Host thread panic: the Rust host thread encountered an unrecoverable
     * panic (e.g. unwinding through {@code catch_unwind}).
     */
    public static final int NATIVE_HOST_PANIC = 205;

    /**
     * ANR (Application Not Responding): the host event loop has not sent a
     * heartbeat for longer than the configured watchdog timeout.
     */
    public static final int NATIVE_ANR = 206;

    /** Manifest signature verification failed. */
    public static final int NATIVE_CODE_SIGNATURE_INVALID = 207;

    /** Code file hash does not match manifest. */
    public static final int NATIVE_CODE_INTEGRITY_FAILED = 208;

    /** Bounded input transport refused an event; the session remains usable. */
    public static final int NATIVE_INPUT_SATURATED = 11;

    /**
     * Get a human-readable message for an error code.
     *
     * @param code The error code
     * @return Human-readable error message
     */
    public static String getMessage(int code) {
        switch (code) {
            case SUCCESS: return "Success";
            case ERR_INIT_FAILED: return "Initialization failed";
            case ERR_INVALID_SURFACE: return "Invalid surface";
            case ERR_INVALID_CONFIG: return "Invalid configuration";
            case ERR_NATIVE_LOAD_FAILED: return "Native library load failed";
            case ERR_INVALID_GAME_ID: return "Invalid game ID or entry point";
            case ERR_NOT_SUPPORTED: return "Device not supported";
            case ERR_SESSION_DESTROYED: return "Session destroyed";
            case ERR_CODE_DIR_NOT_FOUND: return "Code directory not found";
            case ERR_ENTRY_NOT_FOUND: return "Entry point not found";
            case ERR_JS_EXECUTION: return "JavaScript execution error";
            case ERR_CLEANUP_FAILED: return "Terminal resource cleanup failed";
            case ERR_INVALID_ACTIVITY: return "Invalid activity";
            // Native engine errors
            case NATIVE_OUT_OF_MEMORY: return "V8 out of memory";
            case NATIVE_JS_EXECUTION_TIMEOUT: return "JS execution timeout";
            case NATIVE_HOST_PANIC: return "Host thread panic";
            case NATIVE_ANR: return "Application not responding (ANR)";
            case NATIVE_CODE_SIGNATURE_INVALID: return "Code signature invalid";
            case NATIVE_CODE_INTEGRITY_FAILED: return "Code integrity check failed";
            case NATIVE_INPUT_SATURATED: return "Input transport saturated";
            default: return "Unknown error (" + code + ")";
        }
    }

    /**
     * Check if a code indicates success.
     *
     * @param code The error code
     * @return true if code equals SUCCESS
     */
    public static boolean isSuccess(int code) {
        return code == SUCCESS;
    }

    /**
     * Whether an error code represents a recoverable condition.
     * Recoverable errors allow the session to continue running.
     * Non-recoverable errors require the session to be restarted or destroyed.
     *
     * @param code The error code
     * @return true if the error is recoverable
     */
    public static boolean isRecoverable(int code) {
        switch (code) {
            case NATIVE_JS_EXECUTION_TIMEOUT:  // watchdog can restart JS isolate
            case NATIVE_INPUT_SATURATED:
                return true;
            default:
                return false;
        }
    }
}
