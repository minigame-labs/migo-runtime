package com.migo.runtime.internal;

import android.media.AudioRecord;

import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

import org.robolectric.annotation.Implementation;
import org.robolectric.annotation.Implements;

/** Deterministic AudioRecord lifecycle and read behavior for host orchestration tests. */
@Implements(AudioRecord.class)
public final class ShadowAudioRecord {
    static int minBufferSize = 4096;
    static int initialState = AudioRecord.STATE_INITIALIZED;
    static RuntimeException constructorFailure;
    static CountDownLatch readEntered;
    // Held shut to keep a capture read parked inside `read`, so a test can
    // observe manager state while a read is genuinely in flight rather than
    // after it has already returned.
    static CountDownLatch readGate;
    static boolean readSawStart;
    static CountDownLatch releaseEntered;
    static CountDownLatch releaseGate;
    static int liveCount;

    private int state = AudioRecord.STATE_UNINITIALIZED;
    private boolean recording;
    private boolean released;

    @Implementation
    public static int getMinBufferSize(int sampleRateInHz, int channelConfig, int audioFormat) {
        return minBufferSize;
    }

    @Implementation
    public void __constructor__(int audioSource, int sampleRateInHz, int channelConfig,
                                int audioFormat, int bufferSizeInBytes) {
        if (constructorFailure != null) throw constructorFailure;
        state = initialState;
        liveCount++;
    }

    @Implementation
    public int getState() {
        return state;
    }

    @Implementation
    public void startRecording() {
        recording = true;
    }

    @Implementation
    public int getRecordingState() {
        return recording ? AudioRecord.RECORDSTATE_RECORDING : AudioRecord.RECORDSTATE_STOPPED;
    }

    @Implementation
    public int read(byte[] audioData, int offsetInBytes, int sizeInBytes) {
        readSawStart = ShadowNativeBridge.hasEvent("start");
        if (readEntered != null) readEntered.countDown();
        if (readGate != null) {
            try {
                readGate.await(2, TimeUnit.SECONDS);
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
                return 0;
            }
        }
        return 0;
    }

    @Implementation
    public void stop() {
        recording = false;
    }

    @Implementation
    public void release() {
        if (released) return;
        released = true;
        if (releaseEntered != null) releaseEntered.countDown();
        if (releaseGate != null) {
            try {
                releaseGate.await(2, TimeUnit.SECONDS);
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
            }
        }
        if (liveCount > 0) liveCount--;
    }

    static void reset() {
        minBufferSize = 4096;
        initialState = AudioRecord.STATE_INITIALIZED;
        constructorFailure = null;
        readEntered = null;
        readGate = null;
        releaseEntered = null;
        releaseGate = null;
        readSawStart = false;
        liveCount = 0;
    }
}
