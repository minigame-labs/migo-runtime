package com.migo.runtime;

/**
 * Refuses a launch from {@link MigoGameActivity#onCreateRuntimeConfig()}: the game's
 * files are not in place, or the launch names a game this app will not run. The
 * activity reports it through {@link MigoGameActivity#onLaunchFailed} and starts
 * nothing.
 * <p>
 * Checked rather than a {@link MigoException}, so a subclass that refuses launches
 * does not compile against an SDK that would let the refusal escape {@code onCreate}
 * and take the process down.
 */
public final class LaunchRefusedException extends Exception {

    private final int errorCode;

    /**
     * @param errorCode the {@link ErrorCode} {@link MigoGameActivity#onLaunchFailed} receives
     * @param message   why, for the same call
     */
    public LaunchRefusedException(int errorCode, String message) {
        super(message);
        this.errorCode = errorCode;
    }

    public int getErrorCode() {
        return errorCode;
    }
}
