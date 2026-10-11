package com.migo.runtime.callback;

/**
 * Host-provided updates of the content package.
 * <p>
 * A package update is the app's from start to finish: it checks for a new
 * version every time the game starts, a warm start included, downloads what it
 * finds, and restarts the game on it when content asks. Register a handler via
 * {@link com.migo.runtime.GameSession#setUpdateHandler(UpdateHandler)} before
 * the game starts and report each step on the session --
 * {@link com.migo.runtime.GameSession#reportUpdateCheck(boolean)}, then
 * {@link com.migo.runtime.GameSession#reportUpdateReady()} or
 * {@link com.migo.runtime.GameSession#reportUpdateFailed()} -- which content's
 * {@code UpdateManager} hears, a listener registered after the step included.
 * <p>
 * A session with a handler is one whose app checks, so content waits for its
 * report; without one there is no update, and content is told so at once.
 */
public interface UpdateHandler {
    /**
     * Content asked to restart on the version reported ready. Restart the game
     * on it. Called on the main thread.
     */
    void applyUpdate();
}
