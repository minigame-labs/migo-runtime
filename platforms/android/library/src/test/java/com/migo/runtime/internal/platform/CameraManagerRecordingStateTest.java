package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

public final class CameraManagerRecordingStateTest {
    @Test
    public void startSuccessIsOnlyPublishedAfterRecordingStateExists() {
        assertEquals(
                "{\"_error\":{\"errMsg\":\"camera.startRecord:fail recording not started\"}}",
                CameraManager.recordingResultForTests(1));
        assertEquals("{}", CameraManager.recordingResultForTests(2));
    }
}
