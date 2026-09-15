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

        manager.start("{\"frameSize\":1,\"format\":\"pcm\"}");

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

        manager.start("{\"frameSize\":1,\"format\":\"pcm\"}");

        assertFailedStart();
    }

    @Test
    public void cap02_permissionRevokedAfterPublicCheck_releasesLease() {
        manager = new AudioRecorderManager(sessionId, new RevokingActivity());

        manager.start("{\"frameSize\":1,\"format\":\"pcm\"}");

        assertFailedStart();
    }

    @Test
    public void cap02_audioRecordConstructorFailure_releasesLease() {
        ShadowAudioRecord.constructorFailure = new IllegalStateException("constructor failed");
        manager = new AudioRecorderManager(sessionId, activity);

        manager.start("{\"frameSize\":1,\"format\":\"pcm\"}");

        assertFailedStart();
    }

    @Test
    public void cap02_audioRecordNotInitialized_releasesLease() {
        ShadowAudioRecord.initialState = AudioRecord.STATE_UNINITIALIZED;
        manager = new AudioRecorderManager(sessionId, activity);

        manager.start("{\"frameSize\":1,\"format\":\"pcm\"}");

        assertFailedStart();
    }

    @Test
    public void cap02_outputFileOpenFailure_releasesLease() throws Exception {
        File recordings = new File(activity.getCacheDir(), "recordings");
        assertTrue(recordings.createNewFile());
        ShadowAudioRecord.readEntered = new CountDownLatch(1);
        manager = new AudioRecorderManager(sessionId, activity);

        manager.start("{\"frameSize\":1,\"format\":\"pcm\"}");

        assertFailedStart();
    }

    @Test
    public void cap02_mediaRecorderPrepareFailure_releasesLease() {
        ShadowMediaRecorder.prepareFailure = new IllegalStateException("prepare failed");
        manager = new AudioRecorderManager(sessionId, activity);

        manager.start("{}");

        assertFailedStart();
        assertEquals(0, ShadowMediaRecorder.liveCount);
    }

    @Test
    public void cap02_mediaRecorderStartFailure_releasesLease() {
        ShadowMediaRecorder.startFailure = new IllegalStateException("start failed");
        manager = new AudioRecorderManager(sessionId, activity);

        manager.start("{}");

        assertFailedStart();
        assertEquals(0, ShadowMediaRecorder.liveCount);
    }

    @Test
    public void cap02_encodedPipeSetupFailure_releasesLease() {
        ShadowParcelFileDescriptor.createPipeFailure = true;
        manager = new AudioRecorderManager(sessionId, activity);

        manager.start("{\"frameSize\":1,\"format\":\"aac\"}");

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
        manager.start("{\"frameSize\":1,\"format\":\"pcm\"}");
        assertTrue(readEntered.await(2, TimeUnit.SECONDS));

        int secondSession = NEXT_SESSION.getAndIncrement();
        RuntimeGenerationBoundary.registerSession(secondSession);
        Thread stopping = new Thread(manager::stop);
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
