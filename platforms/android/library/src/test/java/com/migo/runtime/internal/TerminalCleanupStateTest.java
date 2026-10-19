package com.migo.runtime.internal;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertSame;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public final class TerminalCleanupStateTest {
    /**
     * A re-entrant attempt is reported as not completed, and does not start a second run.
     *
     * `completed()` is what the caller uses to decide whether ownership was released, and
     * only the completed case was ever asserted -- so a result that always claimed
     * completion looked right. The uncompleted case is the one that matters: it is what a
     * caller sees while the cleanup is still running, and treating it as done would release
     * ownership of resources another thread is still tearing down.
     */
    @Test
    public void aReentrantAttemptIsNotReportedAsCompleted() {
        TerminalCleanupState state = new TerminalCleanupState();
        java.util.List<String> runs = new java.util.ArrayList<>();
        TerminalCleanupState.Result[] reentrant = new TerminalCleanupState.Result[1];

        TerminalCleanupState.Result outer = state.attempt(
                () -> {
                    runs.add("cleanup");
                    // Re-entering while this attempt is running is the contended case,
                    // reached without a second thread because the guard is not reentrant.
                    reentrant[0] = state.attempt(() -> runs.add("nested"), () -> { });
                },
                () -> runs.add("shutdown"));

        assertTrue("the driving attempt completes", outer.completed());
        assertNull(outer.failure());
        assertFalse("a re-entrant attempt is not a completion", reentrant[0].completed());
        assertNull("a refused attempt is not a failure either", reentrant[0].failure());
        assertEquals(
                "a re-entrant attempt must not run the cleanup again",
                java.util.Arrays.asList("cleanup", "shutdown"),
                runs);
    }

    /**
     * A resource that fails to let go does not keep the engine running.
     *
     * Native shutdown ran only after every resource had released, so one release that threw left
     * the engine running the game its player had closed, and nothing retried it -- while the host,
     * told the session was closed, handed the game's files to its installer.
     */
    @Test
    public void aResourceFailureStillShutsTheEngineDown() {
        TerminalCleanupState cleanup = new TerminalCleanupState();
        int[] shutdowns = {0};
        int[] ownershipReleases = {0};

        TerminalCleanupState.Result result = cleanup.attempt(
                () -> {
                    throw new IllegalStateException("a manager would not stop");
                },
                () -> shutdowns[0]++,
                () -> ownershipReleases[0]++);

        assertNotNull(result.failure());
        assertEquals("the engine is shut down anyway", 1, shutdowns[0]);
        assertEquals("ownership stays with the session for a retry", 0, ownershipReleases[0]);
        assertFalse(cleanup.isComplete());
    }

    @Test
    public void aFailedResourceAndAFailedShutdownAreBothReported() {
        IllegalStateException resource = new IllegalStateException("resource");
        IllegalStateException shutdown = new IllegalStateException("shutdown");

        TerminalCleanupState.Result result = new TerminalCleanupState().attempt(
                () -> {
                    throw resource;
                },
                () -> {
                    throw shutdown;
                });

        assertSame(resource, result.failure());
        assertArrayEquals(new Throwable[] {shutdown}, result.failure().getSuppressed());
    }

    @Test
    public void resourceFailureSkipsOwnershipReleaseAndRemainsRetryable() {
        TerminalCleanupState cleanup = new TerminalCleanupState();
        int[] targetedAttempts = {0};
        int[] otherAttempts = {0};
        int[] ownershipReleases = {0};

        TerminalCleanupState.Result firstClose = cleanup.attempt(
                () -> ResourceCleanup.runAll(
                        () -> {
                            targetedAttempts[0]++;
                            throw new IllegalStateException("resource still active");
                        },
                        () -> otherAttempts[0]++),
                () -> {},
                () -> ownershipReleases[0]++);

        assertNotNull(firstClose.failure());
        assertEquals(1, targetedAttempts[0]);
        assertEquals(1, otherAttempts[0]);
        assertEquals(0, ownershipReleases[0]);
        assertFalse(cleanup.isComplete());

        TerminalCleanupState.Result explicitRetry = cleanup.attempt(
                () -> ResourceCleanup.runAll(
                        () -> targetedAttempts[0]++,
                        () -> otherAttempts[0]++),
                () -> {},
                () -> ownershipReleases[0]++);

        assertNull(explicitRetry.failure());
        assertTrue(explicitRetry.completed());
        assertTrue(cleanup.isComplete());
        assertEquals(2, targetedAttempts[0]);
        assertEquals(2, otherAttempts[0]);
        assertEquals(1, ownershipReleases[0]);
    }

    @Test
    public void ownershipFailureRetriesWithoutMarkingCleanupComplete() {
        TerminalCleanupState cleanup = new TerminalCleanupState();
        int[] resources = {0};
        int[] ownership = {0};

        TerminalCleanupState.Result first = cleanup.attempt(
                () -> resources[0]++,
                () -> {},
                () -> {
                    ownership[0]++;
                    throw new IllegalStateException("ownership retained");
                });

        assertNotNull(first.failure());
        assertFalse(cleanup.isComplete());

        TerminalCleanupState.Result retry = cleanup.attempt(
                () -> resources[0]++,
                () -> {},
                () -> ownership[0]++);

        assertNull(retry.failure());
        assertEquals(2, resources[0]);
        assertEquals(2, ownership[0]);
        assertTrue(cleanup.isComplete());
    }

    @Test
    public void ownershipReleaseAttemptsEveryActionAndAggregatesFailure() {
        TerminalCleanupState cleanup = new TerminalCleanupState();
        int[] registry = {0};
        int[] temp = {0};
        int[] sessions = {0};

        TerminalCleanupState.Result result = cleanup.attempt(
                () -> {},
                () -> {},
                () -> {
                    registry[0]++;
                    throw new IllegalStateException("registry release failed");
                },
                () -> temp[0]++,
                () -> sessions[0]++);

        assertNotNull(result.failure());
        assertEquals(1, registry[0]);
        assertEquals(1, temp[0]);
        assertEquals(1, sessions[0]);
        assertFalse(cleanup.isComplete());
    }

    @Test
    public void shutdownFailureSkipsOwnershipReleaseAndRetriesTheWholeBarrier() {
        TerminalCleanupState cleanup = new TerminalCleanupState();
        int[] resources = {0};
        int[] shutdowns = {0};
        int[] ownership = {0};

        TerminalCleanupState.Result first = cleanup.attempt(
                () -> resources[0]++,
                () -> {
                    shutdowns[0]++;
                    throw new IllegalStateException("native join failed");
                },
                () -> ownership[0]++);

        assertNotNull(first.failure());
        assertEquals(1, resources[0]);
        assertEquals(1, shutdowns[0]);
        assertEquals(0, ownership[0]);
        assertFalse(cleanup.isComplete());

        TerminalCleanupState.Result retry = cleanup.attempt(
                () -> resources[0]++,
                () -> shutdowns[0]++,
                () -> ownership[0]++);

        assertNull(retry.failure());
        assertEquals(2, resources[0]);
        assertEquals(2, shutdowns[0]);
        assertEquals(1, ownership[0]);
        assertTrue(cleanup.isComplete());
    }
}
