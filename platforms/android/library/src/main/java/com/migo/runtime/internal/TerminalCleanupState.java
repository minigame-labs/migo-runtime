package com.migo.runtime.internal;

/** Serializes terminal cleanup and releases ownership only after resources are clean. */
public final class TerminalCleanupState {
    public static final class Result {
        private final boolean completed;
        private final RuntimeException failure;

        private Result(boolean completed, RuntimeException failure) {
            this.completed = completed;
            this.failure = failure;
        }

        public boolean completed() {
            return completed;
        }

        public RuntimeException failure() {
            return failure;
        }
    }

    private boolean running;
    private boolean complete;

    /**
     * Makes one caller-driven attempt. Concurrent/reentrant callers do not start another.
     * <p>
     * The engine is shut down whether or not the resources released cleanly. It used to wait
     * for them, so one resource that failed to let go left the engine running the game its
     * player had closed -- for the life of the process, since nothing retries -- and the files
     * it reads were handed back to whoever owns them. Ownership is still released only once
     * both have succeeded: a failed resource stays retained for an explicit retry.
     */
    public Result attempt(
            ResourceCleanup.Action resourceCleanup,
            ResourceCleanup.Action nativeShutdown,
            ResourceCleanup.Action... ownershipRelease) {
        synchronized (this) {
            if (complete) return new Result(true, null);
            if (running) return new Result(false, null);
            running = true;
        }

        RuntimeException failure = null;
        try {
            ResourceCleanup.runAll(resourceCleanup, nativeShutdown);
            ResourceCleanup.runAll(ownershipRelease);
        } catch (RuntimeException error) {
            failure = error;
        } finally {
            synchronized (this) {
                running = false;
                complete = failure == null;
            }
        }
        return new Result(failure == null, failure);
    }

    public synchronized boolean isComplete() {
        return complete;
    }
}
