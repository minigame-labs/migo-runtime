package com.migo.runtime.callback;

/**
 * Host-provided handler for game log entries reported by JS.
 * <p>
 * Register via {@link com.migo.runtime.GameSession#setGameLogHandler(GameLogHandler)}
 * to forward game analytics data to your own backend.
 * <p>
 * When no handler is set, log entries are written to Android logcat.
 */
public interface GameLogHandler {
    /**
     * Called when the game reports a log entry.
     *
     * @param logJson JSON string containing: source, level, key, value -- source
     *                {@code "gameLog"} (getGameLogManager, with commonInfo) or
     *                {@code "realtime"} (getRealtimeLogManager, with filterMsg)
     */
    void onLog(String logJson);
}
