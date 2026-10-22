package com.migo.runtime.internal.platform;

import android.annotation.SuppressLint;
import android.app.Activity;
import android.content.Context;
import android.content.pm.PackageManager;
import android.media.AudioFormat;
import android.media.AudioRecord;
import android.media.MediaRecorder;
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
 * frame data via {@link NativeMethods#onRecorderFrameData}, each with the
 * recording's {@code recordingId}: content took it from the Host id space when
 * it started the recording, and drops what is not of the recording it holds.
 *
 * @hide
 */
public final class AudioRecorderManager implements RuntimeScoped {

    private static final String TAG = "AudioRecorderManager";

    /** What happens to a recording, by number, as the engine numbers it
     *  ({@code shared::services::recorder_event}, {@code MIGO_RECORDER_EVENT_*}). */
    public static final int EVENT_START = 0;
    public static final int EVENT_PAUSE = 1;
    public static final int EVENT_RESUME = 2;
    public static final int EVENT_STOP = 3;
    public static final int EVENT_ERROR = 4;
    public static final int EVENT_INTERRUPTION_BEGIN = 5;
    public static final int EVENT_INTERRUPTION_END = 6;

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
    private String audioMode = "default";
    private int frameSize = 0; // 0 = disabled

    // Recording state: the recording content holds, which every event and frame
    // names; 0 before the first.
    private volatile int recordingId;
    private String outputFilePath;
    private long recordStartTime;
    private long recordedDuration; // accumulated ms across pause/resume cycles
    private Runnable autoStopRunnable;
    private int frameMode = FRAME_NONE;
    /** Set as a recording ends the way the reference counts as normal -- stopped,
     *  or its duration ran out -- so its last frame is sent, flagged. */
    private volatile boolean lastFrameDue;

    /**
     * The runtime this recorder belongs to, which tears it down when it goes.
     * What it reports names its recording rather than the runtime: an id from
     * the Host id space, which a replacement runtime never holds.
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
        if (state.get() != STATE_IDLE) {
            stopInternal(false);
        }
        recordingId = recordingIdOf(optionsJson);
        if (!Permissions.isGranted(activity, Permissions.RECORD_AUDIO)) {
            fail("recorderManager.start:fail auth deny, permission RECORD_AUDIO is required");
            return;
        }

        // One microphone, one owner. Refusing the newcomer keeps the running
        // capture intact; a silent takeover would corrupt it with nothing for the
        // incumbent to observe.
        if (!ExclusiveDeviceArbiter.tryAcquire(ExclusiveDeviceArbiter.MICROPHONE, sessionId)) {
            fail("recorderManager.start:fail in use by another game");
            return;
        }

        boolean ok = false;
        try {
            parseOptions(optionsJson);

            // PCM and WAV are the microphone's samples as they come, which
            // AudioRecord hands over -- with or without frames; the compressed
            // formats are MediaRecorder's.
            String fmt = format.toLowerCase();
            if ("pcm".equals(fmt) || "wav".equals(fmt)) {
                frameMode = FRAME_PCM;
            } else if (frameSize > 0) {
                frameMode = FRAME_ENCODED;
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
            fail("recorderManager.start:fail " + e.getMessage());
        } finally {
            if (!ok) {
                ExclusiveDeviceArbiter.release(ExclusiveDeviceArbiter.MICROPHONE, sessionId);
            }
        }
    }

    /** Pause recording {@code {"recordingId"}}, if it is the one recording. */
    public void pause(String requestJson) {
        if (state.get() != STATE_RECORDING || recordingIdOf(requestJson) != recordingId) return;

        if (frameMode == FRAME_PCM) {
            // AudioRecord: flip state, capture thread will sleep
            state.set(STATE_PAUSED);
            recordedDuration += System.currentTimeMillis() - recordStartTime;
            cancelAutoStop();
            fireEvent(EVENT_PAUSE);
        } else {
            // MediaRecorder-based (encoded mode or encoded-frame mode)
            try {
                mediaRecorder.pause();
                state.set(STATE_PAUSED);
                recordedDuration += System.currentTimeMillis() - recordStartTime;
                cancelAutoStop();
                fireEvent(EVENT_PAUSE);
            } catch (Exception e) {
                fail("recorderManager.pause:fail " + e.getMessage());
            }
        }
    }

    /** Resume recording {@code {"recordingId"}} after pause, if it is the one paused. */
    public void resume(String requestJson) {
        if (state.get() != STATE_PAUSED || recordingIdOf(requestJson) != recordingId) return;

        if (frameMode == FRAME_PCM) {
            state.set(STATE_RECORDING);
            recordStartTime = System.currentTimeMillis();
            scheduleAutoStop();
            fireEvent(EVENT_RESUME);
        } else {
            try {
                mediaRecorder.resume();
                state.set(STATE_RECORDING);
                recordStartTime = System.currentTimeMillis();
                scheduleAutoStop();
                fireEvent(EVENT_RESUME);
            } catch (Exception e) {
                fail("recorderManager.resume:fail " + e.getMessage());
            }
        }
    }

    /** Stop recording {@code {"recordingId"}}, if it is the one under way. */
    public void stop(String requestJson) {
        if (recordingIdOf(requestJson) != recordingId) return;
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
            int source = recordingSource();
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
            fail("recorderManager.start:fail " + e.getMessage());
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
            fail("recorderManager.start:fail auth deny, permission RECORD_AUDIO is required");
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
            fail("recorderManager.start:fail invalid audio parameters");
            return false;
        }

        int frameBufBytes = frameSize * 1024;
        int bufferSize = Math.max(minBufSize, frameBufBytes * 2);

        try {
            audioRecord = new AudioRecord(
                    recordingSource(),
                    sampleRate,
                    channelConfig,
                    audioFmt,
                    bufferSize);

            if (audioRecord.getState() != AudioRecord.STATE_INITIALIZED) {
                fail("recorderManager.start:fail AudioRecord init failed");
                releaseAudioRecord();
                return false;
            }

            outputFilePath = createOutputFilePath();
            outputFileStream = new FileOutputStream(outputFilePath);
            if (isWav()) {
                // Sizes unknown until the recording ends; patched then.
                outputFileStream.write(wavHeader(0));
            }

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
            fail("recorderManager.start:fail " + e.getMessage());
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
        int readSize = chunkBytes > 0 ? Math.min(chunkBytes, 4096) : 4096;
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
                    fail("recorderManager: AudioRecord read error " + bytesRead);
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
                if (chunkBytes > 0) {
                    accOffset = accumulateAndFire(
                            readBuf, bytesRead, accumulator, accOffset, chunkBytes);
                }
            }
        } catch (InterruptedException ignored) {
            // Normal shutdown
        } catch (IOException e) {
            fail("recording write error: " + e.getMessage());
            // Fatal I/O error: tear down + go idle from the main thread (the capture
            // thread can't join itself). Idempotent with an explicit stop().
            mainHandler.post(() -> stopInternal(false));
        }

        if (chunkBytes > 0) flushAccumulator(accumulator, accOffset);
    }

    // ========================================================================
    // Mode 3: Encoded frame mode (MediaRecorder + pipe, encoded frame callbacks)
    // ========================================================================

    private boolean startEncodedFrameMode() {
        try {
            ParcelFileDescriptor[] pipe = ParcelFileDescriptor.createPipe();
            pipeReadFd = pipe[0];
            pipeWriteFd = pipe[1];

            int source = recordingSource();
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
            fail("recorderManager.start:fail " + e.getMessage());
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
                fail("pipe read error: " + e.getMessage());
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
                        sessionId, recordingId, accumulator, chunkBytes, false);
                accOffset = 0;
            }
        }
        return accOffset;
    }

    /**
     * The recording's last frame: what is left, which may be nothing -- a
     * recording of whole frames still ends with one flagged -- when it ended
     * normally; a recording replaced or torn down has no last frame.
     */
    private void flushAccumulator(byte[] accumulator, int accOffset) {
        if (lastFrameDue) {
            NativeMethods.onRecorderFrameData(
                    sessionId, recordingId, accumulator, accOffset, true);
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
        fireEvent(EVENT_START);
    }

    private void attachMediaRecorderListeners() {
        mediaRecorder.setOnInfoListener((mr, what, extra) -> {
            if (what == MediaRecorder.MEDIA_RECORDER_INFO_MAX_DURATION_REACHED) {
                mainHandler.post(() -> stopInternal(true));
            }
        });

        mediaRecorder.setOnErrorListener((mr, what, extra) -> {
            String errMsg = "MediaRecorder error: what=" + what + " extra=" + extra;
            fail(errMsg);
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
        lastFrameDue = notifyStop;

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

        lastFrameDue = false;
        if (outputFilePath != null && frameMode == FRAME_PCM && isWav()) {
            patchWavHeader(new File(outputFilePath));
        }
        if (notifyStop && outputFilePath != null) {
            File file = new File(outputFilePath);
            long fileSize = file.exists() ? file.length() : 0;

            JSONObject result = new JSONObject();
            try {
                result.put("tempFilePath", outputFilePath);
                result.put("duration", recordedDuration);
                result.put("fileSize", fileSize);
            } catch (JSONException ignored) {}

            fireEvent(EVENT_STOP, result);
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
            audioMode = opts.optString("audioMode", "default");
            // Clamp to [0, 1024] KB: 0 disables frame mode; frameSize*1024 is the
            // per-chunk byte allocation (new byte[frameSize*1024]), so an unbounded
            // value would overflow int and/or OOM.
            frameSize = Math.max(0, Math.min(1024, opts.optInt("frameSize", 0)));
        } catch (JSONException e) {
            // Use defaults
        }
    }

    /** The input the recording uses: its audioMode's, which the reference says
     *  wins when content gives both, else its audioSource's. */
    private int recordingSource() {
        switch (audioMode) {
            case "voiceChat":
                return MediaRecorder.AudioSource.VOICE_COMMUNICATION;
            case "voiceRecognition":
                return MediaRecorder.AudioSource.VOICE_RECOGNITION;
            case "recording":
                return unprocessedSupported()
                        ? MediaRecorder.AudioSource.UNPROCESSED
                        : MediaRecorder.AudioSource.MIC;
            default:
                return mapAudioSource(audioSource);
        }
    }

    /** Whether the device records its microphone untouched, as a high-fidelity
     *  recording asks. */
    private boolean unprocessedSupported() {
        android.media.AudioManager audio =
                (android.media.AudioManager) activity.getSystemService(Context.AUDIO_SERVICE);
        return audio != null && "true".equals(audio.getProperty(
                android.media.AudioManager.PROPERTY_SUPPORT_AUDIO_SOURCE_UNPROCESSED));
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
     * The compressed recording a stream of frames is read from: AAC in an ADTS
     * stream, since a pipe cannot seek and MPEG-4 needs to. Android has no MP3
     * encoder; "mp3" records AAC. PCM and WAV never come here (AudioRecord).
     */
    private void configureStreamingFormatAndEncoder(MediaRecorder recorder) {
        recorder.setOutputFormat(MediaRecorder.OutputFormat.AAC_ADTS);
        recorder.setAudioEncoder(MediaRecorder.AudioEncoder.AAC);
    }

    /** The compressed recording written to a file: AAC in MPEG-4, for "aac" and
     *  for "mp3", which Android cannot encode. */
    private void configureFormatAndEncoder(MediaRecorder recorder) {
        recorder.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4);
        recorder.setAudioEncoder(MediaRecorder.AudioEncoder.AAC);
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
                // AudioRecord's samples: raw, or behind a WAV header.
                return isWav() ? ".wav" : ".pcm";
            case FRAME_ENCODED:
                // configureStreamingFormatAndEncoder(): AAC in an ADTS stream.
                return ".aac";
            default: // FRAME_NONE -> configureFormatAndEncoder()
                // MPEG-4 container with an AAC track: Android has no MP3 encoder.
                return ".m4a";
        }
    }

    private boolean isWav() {
        return "wav".equalsIgnoreCase(format);
    }

    /** A canonical 44-byte WAV header for {@code dataBytes} of this recording's
     *  16-bit PCM. */
    private byte[] wavHeader(long dataBytes) {
        java.nio.ByteBuffer header =
                java.nio.ByteBuffer.allocate(44).order(java.nio.ByteOrder.LITTLE_ENDIAN);
        int blockAlign = numberOfChannels * 2;
        header.put(new byte[] {'R', 'I', 'F', 'F'}).putInt((int) (36 + dataBytes))
                .put(new byte[] {'W', 'A', 'V', 'E', 'f', 'm', 't', ' '})
                .putInt(16).putShort((short) 1).putShort((short) numberOfChannels)
                .putInt(sampleRate).putInt(sampleRate * blockAlign)
                .putShort((short) blockAlign).putShort((short) 16)
                .put(new byte[] {'d', 'a', 't', 'a'}).putInt((int) dataBytes);
        return header.array();
    }

    /** The header written as the recording began, given the sizes it ended with. */
    private void patchWavHeader(File file) {
        try (java.io.RandomAccessFile wav = new java.io.RandomAccessFile(file, "rw")) {
            long dataBytes = Math.max(0, wav.length() - 44);
            wav.seek(0);
            wav.write(wavHeader(dataBytes));
        } catch (IOException e) {
            Log.w(TAG, "WAV header not completed: " + e.getMessage());
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

    /** The recording a request names, or 0. */
    private static int recordingIdOf(String requestJson) {
        try {
            return new JSONObject(requestJson).optInt("recordingId", 0);
        } catch (JSONException | NullPointerException e) {
            return 0;
        }
    }

    private void fireEvent(int event) {
        fireEvent(event, new JSONObject());
    }

    /** What happened to the recording, with its id. */
    private void fireEvent(int event, JSONObject payload) {
        try {
            payload.put("recordingId", recordingId);
        } catch (JSONException ignored) {
            // An int always fits.
        }
        NativeMethods.onRecorderEvent(sessionId, event, payload.toString());
    }

    private void fail(String errMsg) {
        JSONObject payload = new JSONObject();
        try {
            payload.put("errMsg", errMsg);
        } catch (JSONException ignored) {
            // A string always fits.
        }
        fireEvent(EVENT_ERROR, payload);
    }
}
