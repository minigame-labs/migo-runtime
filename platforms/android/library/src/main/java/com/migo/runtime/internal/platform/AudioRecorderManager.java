package com.migo.runtime.internal.platform;

import android.annotation.SuppressLint;
import android.app.Activity;
import android.content.pm.PackageManager;
import android.media.AudioFormat;
import android.media.AudioRecord;
import android.media.MediaRecorder;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.os.ParcelFileDescriptor;

import android.util.Log;

import com.migo.runtime.internal.ExclusiveDeviceArbiter;
import com.migo.runtime.internal.NativeMethods;
import com.migo.runtime.internal.RuntimeGenerationBoundary;
import com.migo.runtime.internal.RuntimeScoped;
import com.migo.runtime.internal.ResourceCleanup;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.IOException;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Platform-level audio recorder with three operating modes:
 * <ul>
 *   <li><b>Encoded mode</b> ({@code frameSize == 0}): Uses {@link MediaRecorder} for
 *       compressed output (AAC, etc.). No frame callbacks during recording.</li>
 *   <li><b>PCM frame mode</b> ({@code frameSize > 0} and format is pcm/wav):
 *       Uses {@link AudioRecord} for raw PCM capture with real-time frame callbacks
 *       every {@code frameSize} KB.</li>
 *   <li><b>Encoded frame mode</b> ({@code frameSize > 0} and format is mp3/aac):
 *       Uses {@link MediaRecorder} writing to a {@link ParcelFileDescriptor} pipe.
 *       A background thread reads encoded data from the pipe, accumulates to
 *       {@code frameSize} KB, and fires frame callbacks with encoded data.</li>
 * </ul>
 * <p>
 * Events are sent via {@link NativeMethods#onRecorderEvent} and
 * frame data via {@link NativeMethods#onRecorderFrameData}.
 *
 * @hide
 */
public final class AudioRecorderManager implements RuntimeScoped {

    private static final String TAG = "AudioRecorderManager";

    /** Recording states. */
    private static final int STATE_IDLE = 0;
    private static final int STATE_RECORDING = 1;
    private static final int STATE_PAUSED = 2;

    /** Frame mode sub-types. */
    private static final int FRAME_NONE = 0;
    private static final int FRAME_PCM = 1;
    private static final int FRAME_ENCODED = 2;

    private final int sessionId;
    private final Activity activity;
    private final Handler mainHandler;

    // MediaRecorder (encoded mode + encoded frame mode)
    private MediaRecorder mediaRecorder;

    // AudioRecord (PCM frame mode)
    private AudioRecord audioRecord;

    // Pipe for encoded frame mode
    private ParcelFileDescriptor pipeReadFd;
    private ParcelFileDescriptor pipeWriteFd;

    // Shared frame capture state
    private Thread captureThread;
    private volatile boolean captureRunning;
    private FileOutputStream outputFileStream; // tee to file in encoded-frame mode

    private final AtomicInteger state = new AtomicInteger(STATE_IDLE);

    // Recording options (parsed from JSON)
    private int maxDuration = 60000;
    private int sampleRate = 8000;
    private int numberOfChannels = 2;
    private int encodeBitRate = 48000;
    private String format = "aac";
    private String audioSource = "auto";
    private int frameSize = 0; // 0 = disabled

    // Recording state
    private String outputFilePath;
    private long recordStartTime;
    private long recordedDuration; // accumulated ms across pause/resume cycles
    private Runnable autoStopRunnable;
    private int frameMode = FRAME_NONE;

    /**
     * The runtime this recorder belongs to.
     *
     * <p>Captured once, here, and stamped on every event and frame it reports.
     * Stopping the recorder reports, and the microphone is
     * released by the same path, so the report has to be attributable to the
     * runtime that started it.
     */
    private final RuntimeGenerationBoundary.Token token;

    @Override
    public RuntimeGenerationBoundary.Token runtimeToken() {
        return token;
    }

    public AudioRecorderManager(int sessionId, Activity activity) {
        this.sessionId = sessionId;
        this.token = RuntimeGenerationBoundary.acquire(sessionId);
        this.activity = activity;
        this.mainHandler = new Handler(Looper.getMainLooper());
    }

    /**
     * Start recording with the given options JSON.
     *
     * @param optionsJson JSON with keys: duration, sampleRate, numberOfChannels,
     *                    encodeBitRate, format, frameSize, audioSource
     */
    public void start(String optionsJson) {
        if (!Permissions.isGranted(activity, Permissions.RECORD_AUDIO)) {
            fireEvent("error",
                    "{\"errMsg\":\"recorderManager.start:fail auth deny, permission RECORD_AUDIO is required\"}");
            return;
        }

        if (state.get() != STATE_IDLE) {
            stopInternal(false);
        }

        // One microphone, one owner. Refusing the newcomer keeps the running
        // capture intact; a silent takeover would corrupt it with nothing for the
        // incumbent to observe.
        if (!ExclusiveDeviceArbiter.tryAcquire(ExclusiveDeviceArbiter.MICROPHONE, sessionId)) {
            fireEvent("error",
                    "{\"errMsg\":\"recorderManager.start:fail in use by another game\"}");
            return;
        }

        boolean ok = false;
        try {
            parseOptions(optionsJson);

            if (frameSize > 0) {
                String fmt = format.toLowerCase();
                if ("pcm".equals(fmt) || "wav".equals(fmt)) {
                    frameMode = FRAME_PCM;
                } else {
                    frameMode = FRAME_ENCODED;
                }
            } else {
                frameMode = FRAME_NONE;
            }

            // Hold the lease transactionally: release it on every path that does
            // not reach a valid RECORDING state. Once recording is live the lease
            // belongs to the manager and is released by stopInternal().
            switch (frameMode) {
                case FRAME_PCM:
                    ok = startPcmFrameMode();
                    break;
                case FRAME_ENCODED:
                    ok = startEncodedFrameMode();
                    break;
                default:
                    ok = startEncodedMode();
                    break;
            }
        } catch (Exception e) {
            fireEvent("error",
                    "{\"errMsg\":\"" + escapeJson("recorderManager.start:fail " + e.getMessage()) + "\"}");
        } finally {
            if (!ok) {
                ExclusiveDeviceArbiter.release(ExclusiveDeviceArbiter.MICROPHONE, sessionId);
            }
        }
    }

    /** Pause recording. */
    public void pause() {
        if (state.get() != STATE_RECORDING) return;

        if (frameMode == FRAME_PCM) {
            // AudioRecord: flip state, capture thread will sleep
            state.set(STATE_PAUSED);
            recordedDuration += System.currentTimeMillis() - recordStartTime;
            cancelAutoStop();
            fireEvent("pause", "{}");
        } else {
            // MediaRecorder-based (encoded mode or encoded-frame mode)
            try {
                mediaRecorder.pause();
                state.set(STATE_PAUSED);
                recordedDuration += System.currentTimeMillis() - recordStartTime;
                cancelAutoStop();
                fireEvent("pause", "{}");
            } catch (Exception e) {
                fireEvent("error",
                        "{\"errMsg\":\"" + escapeJson("recorderManager.pause:fail " + e.getMessage()) + "\"}");
            }
        }
    }

    /** Resume recording after pause. */
    public void resume() {
        if (state.get() != STATE_PAUSED) return;

        if (frameMode == FRAME_PCM) {
            state.set(STATE_RECORDING);
            recordStartTime = System.currentTimeMillis();
            scheduleAutoStop();
            fireEvent("resume", "{}");
        } else {
            try {
                mediaRecorder.resume();
                state.set(STATE_RECORDING);
                recordStartTime = System.currentTimeMillis();
                scheduleAutoStop();
                fireEvent("resume", "{}");
            } catch (Exception e) {
                fireEvent("error",
                        "{\"errMsg\":\"" + escapeJson("recorderManager.resume:fail " + e.getMessage()) + "\"}");
            }
        }
    }

    /** Stop recording. */
    public void stop() {
        stopInternal(true);
    }

    /** Release all resources. Call on session destruction. */
    public void destroy() {
        stopInternal(false);
    }

    // ========================================================================
    // Mode 1: Encoded mode (MediaRecorder, no frame callbacks)
    // ========================================================================

    private boolean startEncodedMode() {
        try {
            int source = mapAudioSource(audioSource);
            Log.d(TAG, "startEncodedMode: audioSource=" + audioSource + " mapped=" + source
                    + " format=" + format + " sampleRate=" + sampleRate
                    + " channels=" + numberOfChannels + " bitRate=" + encodeBitRate);
            Log.d(TAG, "RECORD_AUDIO permission: "
                    + (Permissions.isGranted(activity, Permissions.RECORD_AUDIO) ? "GRANTED" : "DENIED"));

            mediaRecorder = new MediaRecorder();
            mediaRecorder.setAudioSource(source);
            configureFormatAndEncoder(mediaRecorder);
            mediaRecorder.setAudioSamplingRate(sampleRate);
            mediaRecorder.setAudioChannels(numberOfChannels);
            mediaRecorder.setAudioEncodingBitRate(encodeBitRate);

            outputFilePath = createOutputFilePath();
            mediaRecorder.setOutputFile(outputFilePath);

            if (maxDuration > 0) {
                mediaRecorder.setMaxDuration(maxDuration);
            }

            attachMediaRecorderListeners();
            mediaRecorder.prepare();
            mediaRecorder.start();

            onRecordingStarted();
            return true;
        } catch (Exception e) {
            state.set(STATE_IDLE);
            cancelAutoStop();
            Log.e(TAG, "startEncodedMode failed", e);
            fireEvent("error",
                    "{\"errMsg\":\"" + escapeJson("recorderManager.start:fail " + e.getMessage()) + "\"}");
            resetMediaRecorder();
            return false;
        }
    }

    // ========================================================================
    // Mode 2: PCM frame mode (AudioRecord, raw PCM frame callbacks)
    // ========================================================================

    // Slim intentionally omits RECORD_AUDIO from its manifest; this guard also
    // handles revocation between the public start() check and construction.
    @SuppressLint("MissingPermission")
    private boolean startPcmFrameMode() {
        // Permission can be revoked after start() validates it.
        if (activity.checkSelfPermission(Permissions.RECORD_AUDIO)
                != PackageManager.PERMISSION_GRANTED) {
            fireEvent("error",
                    "{\"errMsg\":\"recorderManager.start:fail auth deny, permission RECORD_AUDIO is required\"}");
            return false;
        }

        int channelConfig = (numberOfChannels == 1)
                ? AudioFormat.CHANNEL_IN_MONO
                : AudioFormat.CHANNEL_IN_STEREO;
        int audioFmt = AudioFormat.ENCODING_PCM_16BIT;
        int minBufSize = AudioRecord.getMinBufferSize(sampleRate, channelConfig, audioFmt);

        Log.d(TAG, "startPcmFrameMode: audioSource=" + audioSource
                + " sampleRate=" + sampleRate + " channels=" + numberOfChannels
                + " minBufSize=" + minBufSize + " frameSize=" + frameSize);
        Log.d(TAG, "RECORD_AUDIO permission: "
                + (Permissions.isGranted(activity, Permissions.RECORD_AUDIO) ? "GRANTED" : "DENIED"));

        if (minBufSize == AudioRecord.ERROR_BAD_VALUE || minBufSize == AudioRecord.ERROR) {
            fireEvent("error", "{\"errMsg\":\"recorderManager.start:fail invalid audio parameters\"}");
            return false;
        }

        int frameBufBytes = frameSize * 1024;
        int bufferSize = Math.max(minBufSize, frameBufBytes * 2);

        try {
            audioRecord = new AudioRecord(
                    mapAudioSource(audioSource),
                    sampleRate,
                    channelConfig,
                    audioFmt,
                    bufferSize);

            if (audioRecord.getState() != AudioRecord.STATE_INITIALIZED) {
                fireEvent("error", "{\"errMsg\":\"recorderManager.start:fail AudioRecord init failed\"}");
                releaseAudioRecord();
                return false;
            }

            outputFilePath = createOutputFilePath();
            outputFileStream = new FileOutputStream(outputFilePath);

            audioRecord.startRecording();
            // CAP-01: publish STATE_RECORDING before the capture thread is created.
            // Java's Thread.start() establishes a happens-before edge: every write in
            // this thread before thread.start() is visible to the new thread before its
            // first action. Calling onRecordingStarted() here instead of after
            // startCaptureThread() guarantees the capture loop sees STATE_RECORDING on
            // its first state check and never exits on the initial STATE_IDLE.
            onRecordingStarted();
            startCaptureThread(this::pcmCaptureLoop);
            return true;
        } catch (Exception e) {
            state.set(STATE_IDLE);
            cancelAutoStop();
            fireEvent("error",
                    "{\"errMsg\":\"" + escapeJson("recorderManager.start:fail " + e.getMessage()) + "\"}");
            releaseAudioRecord();
            closeOutputStream();
            return false;
        }
    }

    /** PCM capture loop: reads from AudioRecord, writes to file, fires frame callbacks. */
    private void pcmCaptureLoop() {
        final int chunkBytes = frameSize * 1024;
        byte[] accumulator = new byte[chunkBytes];
        int accOffset = 0;
        int readSize = Math.min(chunkBytes, 4096);
        byte[] readBuf = new byte[readSize];

        try {
            while (captureRunning) {
                int currentState = state.get();
                if (currentState == STATE_PAUSED) {
                    Thread.sleep(50);
                    continue;
                }
                if (currentState != STATE_RECORDING) {
                    break;
                }

                int bytesRead = audioRecord.read(readBuf, 0, readBuf.length);
                if (bytesRead < 0) {
                    // Any negative return is a fatal AudioRecord error (dead object,
                    // invalid op, bad value, ...). Stop + clean up instead of
                    // breaking without cleanup or spinning on repeated errors.
                    fireEvent("error",
                            "{\"errMsg\":\"" + escapeJson("recorderManager: AudioRecord read error " + bytesRead) + "\"}");
                    mainHandler.post(() -> stopInternal(false));
                    break;
                }
                if (bytesRead == 0) {
                    continue;
                }

                // Write to file
                if (outputFileStream != null) {
                    outputFileStream.write(readBuf, 0, bytesRead);
                }

                // Accumulate and fire frame callbacks
                accOffset = accumulateAndFire(readBuf, bytesRead, accumulator, accOffset, chunkBytes);
            }
        } catch (InterruptedException ignored) {
            // Normal shutdown
        } catch (IOException e) {
            fireEvent("error",
                    "{\"errMsg\":\"" + escapeJson("recording write error: " + e.getMessage()) + "\"}");
            // Fatal I/O error: tear down + go idle from the main thread (the capture
            // thread can't join itself). Idempotent with an explicit stop().
            mainHandler.post(() -> stopInternal(false));
        }

        // Flush remaining
        flushAccumulator(accumulator, accOffset);
    }

    // ========================================================================
    // Mode 3: Encoded frame mode (MediaRecorder + pipe, encoded frame callbacks)
    // ========================================================================

    private boolean startEncodedFrameMode() {
        try {
            ParcelFileDescriptor[] pipe = ParcelFileDescriptor.createPipe();
            pipeReadFd = pipe[0];
            pipeWriteFd = pipe[1];

            int source = mapAudioSource(audioSource);
            Log.d(TAG, "startEncodedFrameMode: audioSource=" + audioSource + " mapped=" + source
                    + " format=" + format + " sampleRate=" + sampleRate
                    + " channels=" + numberOfChannels + " bitRate=" + encodeBitRate
                    + " frameSize=" + frameSize);
            Log.d(TAG, "RECORD_AUDIO permission: "
                    + (Permissions.isGranted(activity, Permissions.RECORD_AUDIO) ? "GRANTED" : "DENIED"));

            mediaRecorder = new MediaRecorder();
            mediaRecorder.setAudioSource(source);
            // Pipe is not seekable, so we must use streaming-compatible formats.
            // MPEG_4 requires seek (for moov atom), use AAC_ADTS / OGG instead.
            configureStreamingFormatAndEncoder(mediaRecorder);
            mediaRecorder.setAudioSamplingRate(sampleRate);
            mediaRecorder.setAudioChannels(numberOfChannels);
            mediaRecorder.setAudioEncodingBitRate(encodeBitRate);

            // Write encoded data to the pipe instead of a file
            mediaRecorder.setOutputFile(pipeWriteFd.getFileDescriptor());

            if (maxDuration > 0) {
                mediaRecorder.setMaxDuration(maxDuration);
            }

            attachMediaRecorderListeners();

            // Prepare output file for tee
            outputFilePath = createOutputFilePath();
            outputFileStream = new FileOutputStream(outputFilePath);

            mediaRecorder.prepare();
            mediaRecorder.start();

            // Start reading from the pipe
            startCaptureThread(this::encodedCaptureLoop);
            onRecordingStarted();
            return true;
        } catch (Exception e) {
            state.set(STATE_IDLE);
            cancelAutoStop();
            Log.e(TAG, "startEncodedFrameMode failed", e);
            fireEvent("error",
                    "{\"errMsg\":\"" + escapeJson("recorderManager.start:fail " + e.getMessage()) + "\"}");
            resetMediaRecorder();
            closePipe();
            closeOutputStream();
            return false;
        }
    }

    /** Encoded capture loop: reads encoded data from pipe, tees to file, fires frame callbacks. */
    private void encodedCaptureLoop() {
        final int chunkBytes = frameSize * 1024;
        byte[] accumulator = new byte[chunkBytes];
        int accOffset = 0;
        int readSize = Math.min(chunkBytes, 8192);
        byte[] readBuf = new byte[readSize];

        try (InputStream pipeIn = new ParcelFileDescriptor.AutoCloseInputStream(pipeReadFd)) {
            // pipeReadFd is now owned by the AutoCloseInputStream; null our reference
            pipeReadFd = null;

            while (captureRunning) {
                int bytesRead = pipeIn.read(readBuf);
                if (bytesRead <= 0) {
                    // Pipe closed (MediaRecorder stopped) or error
                    break;
                }

                // Tee to output file
                if (outputFileStream != null) {
                    outputFileStream.write(readBuf, 0, bytesRead);
                }

                // Accumulate and fire frame callbacks
                accOffset = accumulateAndFire(readBuf, bytesRead, accumulator, accOffset, chunkBytes);
            }
        } catch (IOException e) {
            if (captureRunning) {
                fireEvent("error",
                        "{\"errMsg\":\"" + escapeJson("pipe read error: " + e.getMessage()) + "\"}");
                // Fatal error mid-recording: tear down from the main thread.
                mainHandler.post(() -> stopInternal(false));
            }
            // else: pipe closed during normal stop, ignore
        }

        // Flush remaining
        flushAccumulator(accumulator, accOffset);
    }

    // ========================================================================
    // Shared frame accumulation
    // ========================================================================

    /**
     * Append data to the accumulator. Each time it fills to {@code chunkBytes},
     * fire a frame callback and reset.
     *
     * @return new accOffset after processing
     */
    private int accumulateAndFire(byte[] data, int dataLen,
                                  byte[] accumulator, int accOffset, int chunkBytes) {
        int srcOffset = 0;
        while (srcOffset < dataLen) {
            int toCopy = Math.min(dataLen - srcOffset, chunkBytes - accOffset);
            System.arraycopy(data, srcOffset, accumulator, accOffset, toCopy);
            accOffset += toCopy;
            srcOffset += toCopy;

            if (accOffset >= chunkBytes) {
                // The JNI call copies synchronously before returning, so the
                // capture-owned accumulator can be reused without a Java clone.
                NativeMethods.onRecorderFrameData(
                        sessionId, token.generation(), accumulator, chunkBytes, false);
                accOffset = 0;
            }
        }
        return accOffset;
    }

    /** Flush any remaining bytes as the last frame without allocating a partial clone. */
    private void flushAccumulator(byte[] accumulator, int accOffset) {
        if (accOffset > 0) {
            NativeMethods.onRecorderFrameData(
                    sessionId, token.generation(), accumulator, accOffset, true);
        }
    }

    // ========================================================================
    // Common helpers
    // ========================================================================

    private void startCaptureThread(Runnable loop) {
        captureRunning = true;
        captureThread = new Thread(loop, "AudioCapture-" + sessionId);
        captureThread.setDaemon(true);
        captureThread.start();
    }

    private void onRecordingStarted() {
        state.set(STATE_RECORDING);
        recordStartTime = System.currentTimeMillis();
        recordedDuration = 0;
        scheduleAutoStop();
        fireEvent("start", "{}");
    }

    private void attachMediaRecorderListeners() {
        mediaRecorder.setOnInfoListener((mr, what, extra) -> {
            if (what == MediaRecorder.MEDIA_RECORDER_INFO_MAX_DURATION_REACHED) {
                mainHandler.post(() -> stopInternal(true));
            }
        });

        mediaRecorder.setOnErrorListener((mr, what, extra) -> {
            String errMsg = "MediaRecorder error: what=" + what + " extra=" + extra;
            fireEvent("error", "{\"errMsg\":\"" + escapeJson(errMsg) + "\"}");
            mainHandler.post(() -> stopInternal(false));
        });
    }

    // ========================================================================
    // Stop / cleanup
    // ========================================================================

    private synchronized void stopInternal(boolean notifyStop) {
        int prevState = state.get();
        if (prevState == STATE_IDLE
                && mediaRecorder == null
                && audioRecord == null
                && captureThread == null
                && outputFileStream == null
                && pipeReadFd == null
                && pipeWriteFd == null) return;

        cancelAutoStop();

        if (prevState == STATE_RECORDING) {
            recordedDuration += System.currentTimeMillis() - recordStartTime;
        }

        try {
            if (frameMode == FRAME_PCM) {
                ResourceCleanup.runAll(
                        this::stopCaptureThread,
                        this::releaseAudioRecord,
                        this::closeOutputStream,
                        this::resetMediaRecorder,
                        this::closePipe);
            } else if (frameMode == FRAME_ENCODED) {
                ResourceCleanup.runAll(
                        this::stopMediaRecorderSafe,
                        this::closePipeWriteFd,
                        this::stopCaptureThread,
                        this::closePipeReadFd,
                        this::closeOutputStream,
                        this::resetMediaRecorder,
                        this::releaseAudioRecord);
            } else {
                ResourceCleanup.runAll(
                        this::stopMediaRecorderSafe,
                        this::resetMediaRecorder,
                        this::releaseAudioRecord,
                        this::stopCaptureThread,
                        this::closeOutputStream,
                        this::closePipe);
            }
        } finally {
            // Keep the logical lease until every hardware-facing cleanup action
            // has been attempted. ResourceCleanup runs all independent actions
            // before propagating failures, and this finally releases the lease
            // even when one of those actions fails.
            ExclusiveDeviceArbiter.release(ExclusiveDeviceArbiter.MICROPHONE, sessionId);
        }
        state.set(STATE_IDLE);

        if (notifyStop && outputFilePath != null) {
            File file = new File(outputFilePath);
            long fileSize = file.exists() ? file.length() : 0;

            JSONObject result = new JSONObject();
            try {
                result.put("tempFilePath", outputFilePath);
                result.put("duration", recordedDuration);
                result.put("fileSize", fileSize);
            } catch (JSONException ignored) {}

            fireEvent("stop", result.toString());
        }
    }

    private void stopMediaRecorderSafe() {
        MediaRecorder recorder = mediaRecorder;
        if (recorder != null) recorder.stop();
    }

    private void stopCaptureThread() {
        captureRunning = false;
        if (captureThread != null) {
            captureThread.interrupt();
            try {
                captureThread.join(1000);
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
                throw new IllegalStateException("interrupted while stopping audio capture",
                        interrupted);
            }
            if (captureThread.isAlive()) {
                throw new IllegalStateException("audio capture thread did not stop");
            }
            captureThread = null;
        }
    }

    private void releaseAudioRecord() {
        AudioRecord recorder = audioRecord;
        if (recorder == null) return;
        ResourceCleanup.runAll(
                () -> {
                    if (recorder.getRecordingState() == AudioRecord.RECORDSTATE_RECORDING) {
                        recorder.stop();
                    }
                },
                () -> {
                    recorder.release();
                    if (audioRecord == recorder) audioRecord = null;
                });
    }

    private void closeOutputStream() {
        FileOutputStream stream = outputFileStream;
        if (stream == null) return;
        ResourceCleanup.runAll(
                () -> {
                    try {
                        stream.flush();
                    } catch (IOException error) {
                        throw new IllegalStateException("failed to flush recorder output", error);
                    }
                },
                () -> {
                    try {
                        stream.close();
                        if (outputFileStream == stream) outputFileStream = null;
                    } catch (IOException error) {
                        throw new IllegalStateException("failed to close recorder output", error);
                    }
                });
    }

    private void resetMediaRecorder() {
        MediaRecorder recorder = mediaRecorder;
        if (recorder == null) return;
        ResourceCleanup.runAll(
                recorder::reset,
                () -> {
                    recorder.release();
                    if (mediaRecorder == recorder) mediaRecorder = null;
                });
    }

    private void closePipe() {
        ResourceCleanup.runAll(this::closePipeReadFd, this::closePipeWriteFd);
    }

    private void closePipeReadFd() {
        ParcelFileDescriptor descriptor = pipeReadFd;
        if (descriptor == null) return;
        try {
            descriptor.close();
            if (pipeReadFd == descriptor) pipeReadFd = null;
        } catch (IOException error) {
            throw new IllegalStateException("failed to close recorder pipe read end", error);
        }
    }

    private void closePipeWriteFd() {
        ParcelFileDescriptor descriptor = pipeWriteFd;
        if (descriptor == null) return;
        try {
            descriptor.close();
            if (pipeWriteFd == descriptor) pipeWriteFd = null;
        } catch (IOException error) {
            throw new IllegalStateException("failed to close recorder pipe write end", error);
        }
    }

    // ========================================================================
    // Options / helpers
    // ========================================================================

    private void parseOptions(String json) {
        if (json == null || json.isEmpty()) return;
        try {
            JSONObject opts = new JSONObject(json);
            maxDuration = opts.optInt("duration", 60000);
            maxDuration = Math.max(0, Math.min(600000, maxDuration));
            sampleRate = opts.optInt("sampleRate", 8000);
            numberOfChannels = opts.optInt("numberOfChannels", 2);
            encodeBitRate = opts.optInt("encodeBitRate", 48000);
            format = opts.optString("format", "aac");
            audioSource = opts.optString("audioSource", "auto");
            // Clamp to [0, 1024] KB: 0 disables frame mode; frameSize*1024 is the
            // per-chunk byte allocation (new byte[frameSize*1024]), so an unbounded
            // value would overflow int and/or OOM.
            frameSize = Math.max(0, Math.min(1024, opts.optInt("frameSize", 0)));
        } catch (JSONException e) {
            // Use defaults
        }
    }

    private int mapAudioSource(String source) {
        if (source == null) return MediaRecorder.AudioSource.DEFAULT;
        switch (source) {
            case "buildInMic":
            case "mic":
                return MediaRecorder.AudioSource.MIC;
            case "camcorder":
                return MediaRecorder.AudioSource.CAMCORDER;
            case "voice_recognition":
                return MediaRecorder.AudioSource.VOICE_RECOGNITION;
            case "voice_communication":
                return MediaRecorder.AudioSource.VOICE_COMMUNICATION;
            case "headsetMic":
                return MediaRecorder.AudioSource.MIC;
            case "auto":
            default:
                return MediaRecorder.AudioSource.DEFAULT;
        }
    }

    /**
     * Configure a streaming-compatible (non-seekable) output format for encoded frame mode.
     * Pipe file descriptors are not seekable, so container formats that require seeking
     * (e.g. MPEG_4) cannot be used.
     */
    private void configureStreamingFormatAndEncoder(MediaRecorder recorder) {
        switch (format.toLowerCase()) {
            case "wav":
            case "pcm":
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    recorder.setOutputFormat(MediaRecorder.OutputFormat.OGG);
                    recorder.setAudioEncoder(MediaRecorder.AudioEncoder.OPUS);
                } else {
                    recorder.setOutputFormat(MediaRecorder.OutputFormat.THREE_GPP);
                    recorder.setAudioEncoder(MediaRecorder.AudioEncoder.AMR_WB);
                }
                break;
            case "mp3":
            case "aac":
            default:
                // AAC_ADTS is a streaming AAC format that does not require seeking.
                recorder.setOutputFormat(MediaRecorder.OutputFormat.AAC_ADTS);
                recorder.setAudioEncoder(MediaRecorder.AudioEncoder.AAC);
                break;
        }
    }

    private void configureFormatAndEncoder(MediaRecorder recorder) {
        switch (format.toLowerCase()) {
            case "mp3":
                recorder.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4);
                recorder.setAudioEncoder(MediaRecorder.AudioEncoder.AAC);
                // Android doesn't natively support MP3 encoding.
                // Use AAC in MP4 container as closest equivalent.
                break;
            case "wav":
            case "pcm":
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    recorder.setOutputFormat(MediaRecorder.OutputFormat.OGG);
                    recorder.setAudioEncoder(MediaRecorder.AudioEncoder.OPUS);
                } else {
                    recorder.setOutputFormat(MediaRecorder.OutputFormat.THREE_GPP);
                    recorder.setAudioEncoder(MediaRecorder.AudioEncoder.AMR_WB);
                }
                break;
            case "aac":
            default:
                recorder.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4);
                recorder.setAudioEncoder(MediaRecorder.AudioEncoder.AAC);
                break;
        }
    }

    private String createOutputFilePath() {
        File cacheDir = activity.getCacheDir();
        File recordDir = new File(cacheDir, "recordings");
        if (!recordDir.exists()) {
            recordDir.mkdirs();
        }

        return new File(recordDir,
                "rec_" + sessionId + "_" + System.currentTimeMillis() + outputExtension())
                .getAbsolutePath();
    }

    /**
     * Extension reflecting the container we ACTUALLY write, not the requested
     * format. Android has no native MP3 encoder, and "encoded" wav/pcm is emulated
     * with a compressed codec, so labelling those files ".mp3"/".wav" would
     * misrepresent their bytes and break downstream format detection.
     */
    private String outputExtension() {
        switch (frameMode) {
            case FRAME_PCM:
                // AudioRecord writes raw PCM (no WAV header) regardless of whether
                // "pcm" or "wav" was requested.
                return ".pcm";
            case FRAME_ENCODED:
                // configureStreamingFormatAndEncoder(): AAC in an ADTS stream.
                return ".aac";
            default: // FRAME_NONE -> configureFormatAndEncoder()
                switch (format.toLowerCase()) {
                    case "wav":
                    case "pcm":
                        // OGG/Opus on Android Q+, else 3GPP/AMR-WB.
                        return (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) ? ".ogg" : ".3gp";
                    case "mp3":
                    case "aac":
                    default:
                        // MPEG-4 container with an AAC track.
                        return ".m4a";
                }
        }
    }

    private void scheduleAutoStop() {
        cancelAutoStop();
        if (maxDuration <= 0) return;

        long remaining = maxDuration - recordedDuration;
        if (remaining <= 0) {
            mainHandler.post(() -> stopInternal(true));
            return;
        }

        autoStopRunnable = () -> stopInternal(true);
        mainHandler.postDelayed(autoStopRunnable, remaining);
    }

    private void cancelAutoStop() {
        if (autoStopRunnable != null) {
            mainHandler.removeCallbacks(autoStopRunnable);
            autoStopRunnable = null;
        }
    }

    private void fireEvent(String eventType, String jsonPayload) {
        NativeMethods.onRecorderEvent(sessionId, token.generation(), eventType, jsonPayload);
    }

    private static String escapeJson(String s) {
        if (s == null) return "";
        return s.replace("\\", "\\\\")
                .replace("\"", "\\\"")
                .replace("\n", "\\n")
                .replace("\r", "\\r")
                .replace("\t", "\\t");
    }
}
