package com.migo.runtime.internal;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import android.app.Activity;
import android.content.pm.PackageManager;
import android.media.AudioRecord;

import com.migo.runtime.internal.platform.AudioRecorderManager;
import com.migo.runtime.internal.platform.Permissions;

import java.io.File;
import java.lang.reflect.Field;
import java.util.List;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;

import org.json.JSONObject;
import org.junit.After;
import org.junit.Before;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.Robolectric;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.RuntimeEnvironment;
import org.robolectric.annotation.Config;
import org.robolectric.shadows.ShadowApplication;

/** Host coverage for Android recorder orchestration; hardware I/O remains device-only. */
@RunWith(RobolectricTestRunner.class)
@Config(sdk = 35, shadows = {
        ShadowNativeBridge.class,
        ShadowAudioRecord.class,
        ShadowMediaRecorder.class,
        ShadowParcelFileDescriptor.class
})
public final class AudioRecorderManagerTest {
    private static final AtomicInteger NEXT_SESSION = new AtomicInteger(4100);
    private int sessionId;
    private Activity activity;
    private AudioRecorderManager manager;

    @Before
    public void setUp() {
        sessionId = NEXT_SESSION.getAndIncrement();
        ExclusiveDeviceArbiter.resetForTests();
        RuntimeGenerationBoundary.registerSession(sessionId);
        ShadowNativeBridge.reset();
        ShadowAudioRecord.reset();
        ShadowMediaRecorder.resetShadow();
        ShadowParcelFileDescriptor.reset();
        activity = Robolectric.buildActivity(Activity.class).create().start().resume().get();
        ShadowApplication shadowApplication =
                org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication());
        shadowApplication.grantPermissions(Permissions.RECORD_AUDIO);
    }

    @After
    public void tearDown() {
        if (manager != null) manager.destroy();
        ExclusiveDeviceArbiter.resetForTests();
        RuntimeGenerationBoundary.unregisterSession(sessionId);
    }

    @Test
    public void cap01_captureThreadReadsOnlyAfterRecordingEventIsPublished() throws Exception {
        CountDownLatch readEntered = new CountDownLatch(1);
        CountDownLatch readGate = new CountDownLatch(1);
        ShadowAudioRecord.readEntered = readEntered;
        ShadowAudioRecord.readGate = readGate;
        manager = new AudioRecorderManager(sessionId, activity);

        manager.start("{\"recordingId\":7,\"frameSize\":1,\"format\":\"pcm\"}");

        assertTrue("capture thread did not reach AudioRecord.read",
                readEntered.await(2, TimeUnit.SECONDS));
        assertTrue("capture read observed before start publication",
                ShadowAudioRecord.readSawStart);
        List<ShadowNativeBridge.Event> events = ShadowNativeBridge.events();
        assertTrue("start must be published before capture can read",
                indexOf(events, "start") >= 0);
        assertEquals(sessionId, ExclusiveDeviceArbiter.ownerForTests(
                ExclusiveDeviceArbiter.MICROPHONE).intValue());
        readGate.countDown();
    }

    @Test
    public void cap02_invalidMinBuffer_releasesAllResourcesAndLease() {
        ShadowAudioRecord.minBufferSize = AudioRecord.ERROR_BAD_VALUE;
        manager = new AudioRecorderManager(sessionId, activity);

        manager.start("{\"recordingId\":7,\"frameSize\":1,\"format\":\"pcm\"}");

        assertFailedStart();
    }

    @Test
    public void cap02_permissionRevokedAfterPublicCheck_releasesLease() {
        manager = new AudioRecorderManager(sessionId, new RevokingActivity());

        manager.start("{\"recordingId\":7,\"frameSize\":1,\"format\":\"pcm\"}");

        assertFailedStart();
    }

    @Test
    public void cap02_audioRecordConstructorFailure_releasesLease() {
        ShadowAudioRecord.constructorFailure = new IllegalStateException("constructor failed");
        manager = new AudioRecorderManager(sessionId, activity);

        manager.start("{\"recordingId\":7,\"frameSize\":1,\"format\":\"pcm\"}");

        assertFailedStart();
    }

    @Test
    public void cap02_audioRecordNotInitialized_releasesLease() {
        ShadowAudioRecord.initialState = AudioRecord.STATE_UNINITIALIZED;
        manager = new AudioRecorderManager(sessionId, activity);

        manager.start("{\"recordingId\":7,\"frameSize\":1,\"format\":\"pcm\"}");

        assertFailedStart();
    }

    @Test
    public void cap02_outputFileOpenFailure_releasesLease() throws Exception {
        File recordings = new File(activity.getCacheDir(), "recordings");
        assertTrue(recordings.createNewFile());
        ShadowAudioRecord.readEntered = new CountDownLatch(1);
        manager = new AudioRecorderManager(sessionId, activity);

        manager.start("{\"recordingId\":7,\"frameSize\":1,\"format\":\"pcm\"}");

        assertFailedStart();
    }

    @Test
    public void cap02_mediaRecorderPrepareFailure_releasesLease() {
        ShadowMediaRecorder.prepareFailure = new IllegalStateException("prepare failed");
        manager = new AudioRecorderManager(sessionId, activity);

        manager.start("{\"recordingId\":7}");

        assertFailedStart();
        assertEquals(0, ShadowMediaRecorder.liveCount);
    }

    @Test
    public void cap02_mediaRecorderStartFailure_releasesLease() {
        ShadowMediaRecorder.startFailure = new IllegalStateException("start failed");
        manager = new AudioRecorderManager(sessionId, activity);

        manager.start("{\"recordingId\":7}");

        assertFailedStart();
        assertEquals(0, ShadowMediaRecorder.liveCount);
    }

    @Test
    public void cap02_encodedPipeSetupFailure_releasesLease() {
        ShadowParcelFileDescriptor.createPipeFailure = true;
        manager = new AudioRecorderManager(sessionId, activity);

        manager.start("{\"recordingId\":7,\"frameSize\":1,\"format\":\"aac\"}");

        assertFailedStart();
        assertEquals(0, ShadowMediaRecorder.liveCount);
    }

    @Test
    public void cap02_leaseRemainsHeldUntilHardwareCleanupCompletes() throws Exception {
        CountDownLatch readEntered = new CountDownLatch(1);
        CountDownLatch readGate = new CountDownLatch(1);
        CountDownLatch releaseEntered = new CountDownLatch(1);
        CountDownLatch releaseGate = new CountDownLatch(1);
        ShadowAudioRecord.readEntered = readEntered;
        ShadowAudioRecord.readGate = readGate;
        ShadowAudioRecord.releaseEntered = releaseEntered;
        ShadowAudioRecord.releaseGate = releaseGate;
        manager = new AudioRecorderManager(sessionId, activity);
        manager.start("{\"recordingId\":7,\"frameSize\":1,\"format\":\"pcm\"}");
        assertTrue(readEntered.await(2, TimeUnit.SECONDS));

        int secondSession = NEXT_SESSION.getAndIncrement();
        RuntimeGenerationBoundary.registerSession(secondSession);
        Thread stopping = new Thread(() -> manager.stop("{\"recordingId\":7}"));
        stopping.start();
        assertTrue(releaseEntered.await(2, TimeUnit.SECONDS));
        assertFalse("lease must remain held while AudioRecord.release is blocked",
                ExclusiveDeviceArbiter.tryAcquire(
                        ExclusiveDeviceArbiter.MICROPHONE, secondSession));

        releaseGate.countDown();
        stopping.join(2000);
        assertFalse(stopping.isAlive());
        assertTrue("lease must be reusable after cleanup",
                ExclusiveDeviceArbiter.tryAcquire(
                        ExclusiveDeviceArbiter.MICROPHONE, secondSession));
        ExclusiveDeviceArbiter.release(ExclusiveDeviceArbiter.MICROPHONE, secondSession);
        RuntimeGenerationBoundary.unregisterSession(secondSession);
        readGate.countDown();
    }

    @Test
    public void recording_reportsNameTheirRecordingAndOnlyItsOwnRequestsReachIt() throws Exception {
        CountDownLatch readEntered = new CountDownLatch(1);
        ShadowAudioRecord.readEntered = readEntered;
        manager = new AudioRecorderManager(sessionId, activity);
        manager.start("{\"recordingId\":7,\"frameSize\":1,\"format\":\"pcm\"}");
        assertTrue(readEntered.await(2, TimeUnit.SECONDS));

        // A request for a recording that is not this one is not for it.
        manager.pause("{\"recordingId\":8}");
        manager.stop("{\"recordingId\":8}");
        assertEquals(1, state());
        assertEquals(-1, indexOf(ShadowNativeBridge.events(), "pause"));

        manager.pause("{\"recordingId\":7}");
        manager.stop("{\"recordingId\":7}");
        assertEquals(0, state());
        List<ShadowNativeBridge.Event> events = ShadowNativeBridge.events();
        int stop = indexOf(events, "stop");
        assertTrue("the recording stops", stop >= 0);
        JSONObject stopped = new JSONObject(events.get(stop).payload);
        assertEquals(7, stopped.getInt("recordingId"));
        assertTrue("the stop hands over its file", stopped.has("tempFilePath"));
        assertEquals(7, new JSONObject(events.get(indexOf(events, "start")).payload)
                .getInt("recordingId"));
    }

    @Test
    public void recording_aFailedStartNamesTheRecordingThatFailed() throws Exception {
        ShadowAudioRecord.minBufferSize = AudioRecord.ERROR_BAD_VALUE;
        manager = new AudioRecorderManager(sessionId, activity);

        manager.start("{\"recordingId\":9,\"frameSize\":1,\"format\":\"pcm\"}");

        List<ShadowNativeBridge.Event> events = ShadowNativeBridge.events();
        int error = indexOf(events, "error");
        assertTrue(error >= 0);
        assertEquals(9, new JSONObject(events.get(error).payload).optInt("recordingId"));
    }

    @Test
    public void recording_endsWithAFlaggedLastFrameEvenWithNothingLeftOver() throws Exception {
        CountDownLatch readEntered = new CountDownLatch(1);
        ShadowAudioRecord.readEntered = readEntered;
        manager = new AudioRecorderManager(sessionId, activity);
        manager.start("{\"recordingId\":7,\"frameSize\":1,\"format\":\"PCM\"}");
        assertTrue(readEntered.await(2, TimeUnit.SECONDS));

        manager.stop("{\"recordingId\":7}");

        List<ShadowNativeBridge.Event> events = ShadowNativeBridge.events();
        int last = indexOfPayload(events, "frame", "7:0:true");
        assertTrue("the recording's end is a flagged frame: " + payloads(events), last >= 0);
        assertTrue("before the recording's STOP", last < indexOf(events, "stop"));
    }

    @Test
    public void recording_aReplacedRecordingHasNoLastFrame() throws Exception {
        CountDownLatch readEntered = new CountDownLatch(1);
        ShadowAudioRecord.readEntered = readEntered;
        manager = new AudioRecorderManager(sessionId, activity);
        manager.start("{\"recordingId\":7,\"frameSize\":1,\"format\":\"PCM\"}");
        assertTrue(readEntered.await(2, TimeUnit.SECONDS));

        manager.destroy();

        assertEquals(-1, indexOf(ShadowNativeBridge.events(), "frame"));
        assertEquals(-1, indexOf(ShadowNativeBridge.events(), "stop"));
    }

    @Test
    public void recording_wavIsTheMicrophonesSamplesBehindAWavHeader() throws Exception {
        CountDownLatch readEntered = new CountDownLatch(1);
        ShadowAudioRecord.readEntered = readEntered;
        manager = new AudioRecorderManager(sessionId, activity);
        manager.start("{\"recordingId\":7,\"format\":\"wav\",\"sampleRate\":16000,"
                + "\"numberOfChannels\":1}");
        assertTrue("wav records through AudioRecord", readEntered.await(2, TimeUnit.SECONDS));

        manager.stop("{\"recordingId\":7}");

        List<ShadowNativeBridge.Event> events = ShadowNativeBridge.events();
        JSONObject stopped = new JSONObject(events.get(indexOf(events, "stop")).payload);
        File wav = new File(stopped.getString("tempFilePath"));
        assertTrue(wav.getName(), wav.getName().endsWith(".wav"));
        byte[] header = java.nio.file.Files.readAllBytes(wav.toPath());
        assertEquals(44, header.length);
        assertEquals(stopped.getLong("fileSize"), header.length);
        assertEquals("RIFF", new String(header, 0, 4, "US-ASCII"));
        assertEquals("WAVE", new String(header, 8, 4, "US-ASCII"));
        java.nio.ByteBuffer le = java.nio.ByteBuffer.wrap(header).order(java.nio.ByteOrder.LITTLE_ENDIAN);
        assertEquals(1, le.getShort(22));
        assertEquals(16000, le.getInt(24));
        assertEquals(0, le.getInt(40));
    }

    private static int indexOfPayload(List<ShadowNativeBridge.Event> events, String type, String payload) {
        for (int i = 0; i < events.size(); i++) {
            if (type.equals(events.get(i).type) && payload.equals(events.get(i).payload)) return i;
        }
        return -1;
    }

    private static String payloads(List<ShadowNativeBridge.Event> events) {
        StringBuilder out = new StringBuilder();
        for (ShadowNativeBridge.Event event : events) out.append(event.type).append('=').append(event.payload).append(' ');
        return out.toString();
    }

    private void assertFailedStart() {
        List<ShadowNativeBridge.Event> events = ShadowNativeBridge.events();
        assertTrue("failed start must report an error", indexOf(events, "error") >= 0);
        assertEquals(0, state());
        assertNull(field("audioRecord"));
        assertNull(field("mediaRecorder"));
        assertNull(field("outputFileStream"));
        assertNull(field("captureThread"));
        assertNull(field("pipeReadFd"));
        assertNull(field("pipeWriteFd"));
        assertEquals(0, ShadowAudioRecord.liveCount);
        assertEquals(0, ShadowMediaRecorder.liveCount);
        assertNull(ExclusiveDeviceArbiter.ownerForTests(ExclusiveDeviceArbiter.MICROPHONE));
        int secondSession = NEXT_SESSION.getAndIncrement();
        RuntimeGenerationBoundary.registerSession(secondSession);
        assertTrue("second host must acquire after failed start",
                ExclusiveDeviceArbiter.tryAcquire(
                        ExclusiveDeviceArbiter.MICROPHONE, secondSession));
        ExclusiveDeviceArbiter.release(ExclusiveDeviceArbiter.MICROPHONE, secondSession);
        RuntimeGenerationBoundary.unregisterSession(secondSession);
    }

    private int state() {
        return ((AtomicInteger) fieldValue("state")).get();
    }

    private Object field(String name) {
        return fieldValue(name);
    }

    private Object fieldValue(String name) {
        try {
            Field field = AudioRecorderManager.class.getDeclaredField(name);
            field.setAccessible(true);
            return field.get(manager);
        } catch (ReflectiveOperationException error) {
            throw new AssertionError(error);
        }
    }

    private static int indexOf(List<ShadowNativeBridge.Event> events, String type) {
        for (int i = 0; i < events.size(); i++) {
            if (type.equals(events.get(i).type)) return i;
        }
        return -1;
    }

    private static final class RevokingActivity extends Activity {
        private int checks;

        @Override
        public int checkSelfPermission(String permission) {
            return checks++ == 0
                    ? PackageManager.PERMISSION_GRANTED
                    : PackageManager.PERMISSION_DENIED;
        }
    }
}
