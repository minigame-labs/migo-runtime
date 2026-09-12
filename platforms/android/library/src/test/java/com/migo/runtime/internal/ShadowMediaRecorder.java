package com.migo.runtime.internal;

import android.media.MediaRecorder;

import java.io.FileDescriptor;

import org.robolectric.annotation.Implementation;
import org.robolectric.annotation.Implements;

/** Controllable MediaRecorder lifecycle for encoded-mode rollback tests. */
@Implements(MediaRecorder.class)
public final class ShadowMediaRecorder {
    static RuntimeException prepareFailure;
    static RuntimeException startFailure;
    static int liveCount;

    private boolean released;
    private MediaRecorder.OnInfoListener infoListener;
    private MediaRecorder.OnErrorListener errorListener;

    @Implementation
    public void __constructor__() {
        liveCount++;
    }

    @Implementation
    public void setAudioSource(int audioSource) {}

    @Implementation
    public void setOutputFormat(int outputFormat) {}

    @Implementation
    public void setAudioEncoder(int audioEncoder) {}

    @Implementation
    public void setAudioSamplingRate(int sampleRate) {}

    @Implementation
    public void setAudioChannels(int numChannels) {}

    @Implementation
    public void setAudioEncodingBitRate(int bitRate) {}

    @Implementation
    public void setOutputFile(String path) {}

    @Implementation
    public void setOutputFile(FileDescriptor fd) {}

    @Implementation
    public void setMaxDuration(int maxDurationMs) {}

    @Implementation
    public void setOnInfoListener(MediaRecorder.OnInfoListener listener) {
        infoListener = listener;
    }

    @Implementation
    public void setOnErrorListener(MediaRecorder.OnErrorListener listener) {
        errorListener = listener;
    }

    @Implementation
    public void prepare() {
        if (prepareFailure != null) throw prepareFailure;
    }

    @Implementation
    public void start() {
        if (startFailure != null) throw startFailure;
    }

    @Implementation
    public void pause() {}

    @Implementation
    public void resume() {}

    @Implementation
    public void stop() {}

    @Implementation
    public void reset() {}

    @Implementation
    public void release() {
        if (!released) {
            released = true;
            liveCount--;
        }
    }

    static void resetShadow() {
        prepareFailure = null;
        startFailure = null;
        liveCount = 0;
    }
}
