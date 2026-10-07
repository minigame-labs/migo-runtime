package com.migo.runtime.internal;

import static org.junit.Assert.fail;

import java.lang.reflect.Method;

/**
 * Steady-state allocation gate for paths that allocate inside
 * the JVM.
 *
 * <p>The Rust probe in {@code engine/testing/alloc-probe} counts what reaches the
 * system allocator, and a Rust allocator observes nothing the JVM allocates. The
 * Android half of the BLE notification path is entirely JVM allocation --
 * capturing lambdas, a connection wrapper, two formatted UUID strings -- so it
 * needs an instrument of its own. This is that instrument, and it keeps the
 * properties that make the Rust one a gate rather than a decoration:
 *
 * <ul>
 *   <li><b>The instrument proves itself before a zero is trusted.</b> A JVM
 *       without per-thread allocation counting reports no growth for every
 *       burst, which is indistinguishable from a path that allocates nothing.
 *       Every burst therefore performs a known allocation first and fails if the
 *       counter does not move. This is the failure mode that would otherwise
 *       turn every gate here into a permanent silent pass.</li>
 *   <li><b>Warm-up is mandatory and non-zero</b>, and so is the measured span.
 *       First use is not steady state -- classes load, call sites link, bounded
 *       caches fill -- and a zero-length window is vacuous.</li>
 *   <li><b>The instrument's own cost is measured, not assumed.</b> See
 *       {@link #measure}.</li>
 * </ul>
 *
 * <p><b>Why reflection.</b> These tests compile against {@code android.jar},
 * which has no {@code java.lang.management}; they <em>run</em> on a real JDK,
 * which does. Naming the type would not compile, so the counter is reached by
 * name at runtime. A JDK that does not have it fails the burst rather than
 * skipping it: a skipped gate and a passing gate are the same green tick.
 */
public final class AllocationProbe {
    private AllocationProbe() {}

    private static final Runnable NOTHING = () -> {};

    /**
     * One burst at a time in a test binary.
     *
     * <p>The counter is per thread, so concurrent bursts would not corrupt each
     * other's numbers -- but {@link AllocationProbeControlTest} proves the
     * self-check fires by switching the counter <em>off</em>, and that switch is
     * process-wide. Serialising here rather than asking each call site to
     * remember is what keeps that control from turning another burst red.
     * Reentrant, so the control can hold it across its own assertion.
     */
    static final Object ONE_BURST_AT_A_TIME = new Object();

    /** Kept reachable so the self-check's allocation cannot be optimised away. */
    private static volatile byte[] sink;

    private static Method getThreadAllocatedBytes;
    private static Object threadBean;
    private static Object[] currentThreadId;

    /**
     * Run {@code body} {@code warmup + measured} times and fail if the measured
     * iterations allocate.
     *
     * @param path     quoted verbatim in the failure, so a report names the path
     *                 rather than the test
     * @param warmup   iterations before the window opens; must be non-zero
     * @param measured iterations inside the window; must be non-zero
     */
    public static void assertNoSteadyStateAllocation(
            String path, int warmup, int measured, Runnable body) {
        synchronized (ONE_BURST_AT_A_TIME) {
            burst(path, warmup, measured, body);
        }
    }

    private static void burst(String path, int warmup, int measured, Runnable body) {
        if (warmup <= 0) {
            fail(path + ": a burst without warm-up measures first use, not steady state");
        }
        if (measured <= 0) {
            fail(path + ": a burst that measures no iteration cannot fail");
        }
        resolveCounter(path);
        warmTheInstrument();
        requireTheInstrumentWorks(path);

        for (int i = 0; i < warmup; i++) {
            body.run();
        }
        // The control runs warm too, so what it measures is two counter reads
        // rather than the first linkage of an empty lambda.
        measure(1, NOTHING);

        long control = measure(measured, NOTHING);
        long observed = measure(measured, body);
        long allocated = observed - control;

        if (allocated != 0) {
            fail(path + ": " + allocated + " byte(s) allocated over " + measured
                    + " measured iteration(s) (" + observed + " observed, " + control
                    + " for the instrument itself). The path must have zero"
                    + " steady-state allocation.");
        }
    }

    /**
     * Bytes this thread allocated across {@code iterations} runs of {@code body},
     * including the cost of the two counter reads themselves.
     *
     * <p>That inclusion is why the caller subtracts a control: reading the
     * counter goes through {@link Method#invoke}, whose own argument handling may
     * allocate, and a gate that assumed the instrument were free would report the
     * instrument. Running the identical read pair around an empty body measures
     * exactly that cost, so the difference is the body's and nothing else.
     */
    private static long measure(int iterations, Runnable body) {
        long before = allocatedBytes();
        for (int i = 0; i < iterations; i++) {
            body.run();
        }
        return allocatedBytes() - before;
    }

    /**
     * Allocate something the JVM cannot fold away and require the counter to see
     * it.
     *
     * <p>Published to a static field because escape analysis may scalarise an
     * object that provably never escapes, and a scalarised allocation is one the
     * counter would not see. A failure here is the instrument, never the path
     * under test.
     */
    private static void requireTheInstrumentWorks(String path) {
        long before = allocatedBytes();
        sink = new byte[4096];
        long observed = allocatedBytes() - before;
        if (observed < 4096) {
            fail(path + ": per-thread allocation counting reported " + observed
                    + " bytes for a 4096-byte allocation, so a zero from this probe would"
                    + " mean nothing.");
        }
    }

    /**
     * Run the counter read until it stops doing one-time work.
     *
     * <p>Found by measurement, and it cost a real diagnosis: a burst over a path
     * that provably allocated nothing reported eleven kilobytes. The path was
     * innocent -- {@link Method#invoke} starts out on a native accessor and
     * <em>spins a generated class</em> for the call site once it has been used
     * about fifteen times, and that class landed inside the measured window. The
     * body's warm-up cannot cover it, because the instrument is not the body.
     *
     * <p>The threshold is JDK-internal, so this passes it by an order of
     * magnitude rather than matching it. A JDK that resolves reflection some
     * other way simply does the reads sooner.
     */
    private static void warmTheInstrument() {
        for (int i = 0; i < 256; i++) {
            allocatedBytes();
        }
    }

    private static long allocatedBytes() {
        try {
            return (Long) getThreadAllocatedBytes.invoke(threadBean, currentThreadId);
        } catch (ReflectiveOperationException failure) {
            throw new AssertionError("the allocation counter became unreachable", failure);
        }
    }

    /**
     * Turn the counter off and on again, for the control that proves a burst
     * refuses to report zero when the instrument is silent.
     *
     * <p>Package-private and named for its one caller: nothing else may leave
     * the process without a working counter.
     */
    static void setCountingEnabledForControl(boolean enabled) {
        try {
            Class<?> counting = Class.forName("com.sun.management.ThreadMXBean");
            counting.getMethod("setThreadAllocatedMemoryEnabled", boolean.class)
                    .invoke(threadBean, enabled);
        } catch (ReflectiveOperationException failure) {
            throw new AssertionError("the allocation counter could not be switched", failure);
        }
    }

    /** Resolve the counter while it still works, before the control disables it. */
    static void resolveForControl() {
        resolveCounter("AllocationProbe control");
    }

    private static synchronized void resolveCounter(String path) {
        if (getThreadAllocatedBytes != null) return;
        try {
            Class<?> factory = Class.forName("java.lang.management.ManagementFactory");
            Class<?> counting = Class.forName("com.sun.management.ThreadMXBean");
            Object bean = factory.getMethod("getThreadMXBean").invoke(null);
            if (!counting.isInstance(bean)) {
                fail(path + ": this JVM's ThreadMXBean does not count allocation, so the"
                        + " gate cannot run. A skip here is the failure it exists to"
                        + " prevent.");
            }
            if (!(Boolean) counting.getMethod("isThreadAllocatedMemorySupported").invoke(bean)) {
                fail(path + ": per-thread allocation measurement is unsupported here.");
            }
            if (!(Boolean) counting.getMethod("isThreadAllocatedMemoryEnabled").invoke(bean)) {
                counting.getMethod("setThreadAllocatedMemoryEnabled", boolean.class)
                        .invoke(bean, true);
            }
            Method read = counting.getMethod("getThreadAllocatedBytes", long.class);
            read.setAccessible(true);
            threadBean = bean;
            currentThreadId = new Object[] {Thread.currentThread().getId()};
            getThreadAllocatedBytes = read;
        } catch (ReflectiveOperationException failure) {
            throw new AssertionError(
                    path + ": the JDK allocation counter could not be reached", failure);
        }
    }
}
