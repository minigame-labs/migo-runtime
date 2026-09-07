package com.migo.runtime.callback;

import com.migo.runtime.MigoException;

/**
 * Unified listener for game session events.
 * <p>
 * Implement this interface to receive game state, error, and optional
 * loading/lifecycle notifications from a GameSession.
 * <p>
 * Only three methods require implementation:
 * <ul>
 *   <li>{@link #onGameReady()} — game finished loading</li>
 *   <li>{@link #onGameExit(int)} — game exited</li>
 *   <li>{@link #onError(MigoException)} — runtime error</li>
 * </ul>
 * <p>
 * All other methods have default (no-op) implementations and are optional.
 *
 * <pre>{@code
 * session.setListener(new GameSessionListener() {
 *     @Override
 *     public void onGameReady() {
 *         hideLoadingScreen();
 *     }
 *
 *     @Override
 *     public void onGameExit(int exitCode) {
 *         finish();
 *     }
 *
 *     @Override
 *     public void onError(MigoException exception) {
 *         if (!exception.isRecoverable()) {
 *             showErrorDialog(exception.getMessage());
 *         }
 *     }
 * });
 * }</pre>
 *
 * All callbacks are invoked on the main thread.
 *
 */
public interface GameSessionListener {

    // ==================== Core (required) ====================

    /**
     * Called when the game has finished loading and is ready to run.
     */
    void onGameReady();

    /**
     * Called when the game has exited.
     *
     * @param exitCode The exit code (0 for normal exit, non-zero for errors)
     */
    void onGameExit(int exitCode);

    /**
     * Called when the game session encounters an error.
     * Called on the main thread.
     *
     * @param exception structured error with code, message, cause, and recoverability
     */
    void onError(MigoException exception);

    // ==================== Loading (optional) ====================

    /**
     * Called when the game requests to show a loading screen.
     */
    default void onLoadingStart() {}

    /**
     * Called when loading is complete and the loading screen can be hidden.
     */
    default void onLoadingEnd() {}

    /**
     * Called periodically during loading with progress information.
     *
     * @param progress Progress value from 0.0 to 1.0
     * @param message  Optional message describing current loading step
     */
    default void onLoadingProgress(float progress, String message) {}

    // ==================== Lifecycle (optional) ====================

    /**
     * Called when the session is paused (e.g., app goes to background).
     */
    default void onPaused() {}

    /**
     * Called when the session is resumed (e.g., app comes to foreground).
     */
    default void onResumed() {}

    /**
     * Called when the session is being destroyed.
     */
    default void onDestroyed() {}

    // ==================== Surface (optional) ====================

    /**
     * Called when the engine gave up a Surface it could not present to.
     * <p>
     * Not the same as the app taking its own Surface back: this is reported only for a
     * Surface the app still holds and still believes is live. Nothing will be drawn
     * until another is attached, so the recovery is to call
     * {@code session.updateSurface(surface)} once the {@code SurfaceHolder} has a valid
     * one — or, if it does not, to wait for {@code surfaceCreated} and attach then.
     *
     * @param reason 0 unknown, 1 host destroyed, 2 device lost, 3 platform error
     */
    default void onSurfaceLost(int reason) {}
}
