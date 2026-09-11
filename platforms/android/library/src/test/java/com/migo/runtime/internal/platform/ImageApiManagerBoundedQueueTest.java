package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.RejectedExecutionException;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import org.junit.Test;

public final class ImageApiManagerBoundedQueueTest {
    @Test
    public void compression_executor_rejects_after_bounded_queue_is_full() throws Exception {
        ThreadPoolExecutor workers = (ThreadPoolExecutor) ImageApiManager.IMAGE_WORKERS;
        CountDownLatch release = new CountDownLatch(1);
        List<Runnable> blockers = new ArrayList<>();
        for (int i = 0; i < 2; i++) {
            Runnable blocker = () -> {
                try {
                    release.await();
                } catch (InterruptedException interrupted) {
                    Thread.currentThread().interrupt();
                }
            };
            blockers.add(blocker);
            workers.execute(blocker);
        }
        assertEquals(ImageApiManager.COMPRESS_QUEUE_CAPACITY,
                workers.getQueue().remainingCapacity());
        for (int i = 0; i < ImageApiManager.COMPRESS_QUEUE_CAPACITY; i++) {
            workers.execute(() -> {});
        }
        try {
            workers.execute(() -> {});
            throw new AssertionError("queue admission unexpectedly unbounded");
        } catch (RejectedExecutionException expected) {
            // The request is settled by compressAsync's rejection path.
        } finally {
            release.countDown();
            workers.awaitTermination(1, TimeUnit.MILLISECONDS);
        }
    }

    @Test
    public void decoded_pixel_permit_is_held_and_released() {
        int permits = 17;
        assertTrue(ImageApiManager.tryReserveDecodedPixels(permits));
        assertFalse(ImageApiManager.tryReserveDecodedPixels(ImageApiManager.MAX_DECODED_PIXELS));
        ImageApiManager.releaseDecodedPixels(permits);
        assertTrue(ImageApiManager.tryReserveDecodedPixels(permits));
        ImageApiManager.releaseDecodedPixels(permits);
    }
}
