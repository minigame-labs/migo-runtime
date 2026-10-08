package com.migo.runtime.internal.platform;

import android.content.Context;
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.os.Build;

import com.migo.runtime.internal.NativeMethods;

/**
 * Manages audio focus and dispatches audio interruption events to the game.
 * <p>
 * Automatically listens for system audio focus changes (e.g., incoming calls,
 * other apps playing audio) and triggers the corresponding JS callbacks.
 * <p>
 * Compatible with Android API 26+.
 *
 * @hide
 */
public final class AudioFocusManager {

    private final int sessionId;
    private final AudioManager audioManager;
    private final AudioManager.OnAudioFocusChangeListener focusListener;
    private Object focusRequest; // AudioFocusRequest on API 26+
    private boolean interrupted = false;

    public AudioFocusManager(int sessionId, Context context) {
        this.sessionId = sessionId;
        this.audioManager = (AudioManager) context.getSystemService(Context.AUDIO_SERVICE);
        this.focusListener = this::onAudioFocusChange;
    }

    /**
     * Request audio focus and start listening for interruptions.
     */
    public void start() {
        if (audioManager == null) return;

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            AudioFocusRequest request = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                    .setAudioAttributes(new AudioAttributes.Builder()
                            .setUsage(AudioAttributes.USAGE_GAME)
                            .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                            .build())
                    .setOnAudioFocusChangeListener(focusListener)
                    .build();
            audioManager.requestAudioFocus(request);
            this.focusRequest = request;
        } else {
            audioManager.requestAudioFocus(
                    focusListener,
                    AudioManager.STREAM_MUSIC,
                    AudioManager.AUDIOFOCUS_GAIN
            );
        }
    }

    /**
     * Re-request audio focus if currently interrupted.
     * <p>
     * After {@code AUDIOFOCUS_LOSS} (permanent loss), Android will not
     * automatically send {@code AUDIOFOCUS_GAIN} back. Call this on resume
     * so the game can recover audio after e.g. a phone call.
     */
    public void requestFocusIfNeeded() {
        if (audioManager == null || !interrupted) return;

        int result;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && focusRequest != null) {
            result = audioManager.requestAudioFocus((AudioFocusRequest) focusRequest);
        } else {
            result = audioManager.requestAudioFocus(
                    focusListener,
                    AudioManager.STREAM_MUSIC,
                    AudioManager.AUDIOFOCUS_GAIN
            );
        }

        if (result == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
            // Focus granted immediately; the listener won't fire AUDIOFOCUS_GAIN
            // in this case, so trigger the end event manually.
            interrupted = false;
            NativeMethods.onAudioInterruptionEnd(sessionId);
        }
        // If not granted, the listener will fire AUDIOFOCUS_GAIN later.
    }

    /**
     * Abandon audio focus and stop listening.
     */
    public void stop() {
        if (audioManager == null) return;

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && focusRequest != null) {
            audioManager.abandonAudioFocusRequest((AudioFocusRequest) focusRequest);
            focusRequest = null;
        } else {
            audioManager.abandonAudioFocus(focusListener);
        }
    }

    private void onAudioFocusChange(int focusChange) {
        switch (focusChange) {
            case AudioManager.AUDIOFOCUS_LOSS:
            case AudioManager.AUDIOFOCUS_LOSS_TRANSIENT:
            case AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK:
                if (!interrupted) {
                    interrupted = true;
                    NativeMethods.onAudioInterruptionBegin(sessionId);
                }
                break;
            case AudioManager.AUDIOFOCUS_GAIN:
                if (interrupted) {
                    interrupted = false;
                    NativeMethods.onAudioInterruptionEnd(sessionId);
                }
                break;
        }
    }
}
