package com.migo.runtime.internal;

import java.nio.ByteBuffer;

import org.junit.Test;

/**
 * Metadata rejected by the Java callback facade must not reach the raw JNI
 * declaration. These host-JVM tests deliberately run without libmigo loaded:
 * a native call would throw {@link UnsatisfiedLinkError}.
 */
public final class NativeMethodsCameraFrameValidationTest {

    private static ByteBuffer directPlane() {
        return ByteBuffer.allocateDirect(1);
    }

    @Test
    public void rejectsOversizedDimensionsBeforeCallingNative() {
        ByteBuffer plane = directPlane();

        NativeMethods.onCameraFrameData(1, 0L, 1,
                plane, 0, 1, 1,
                plane, 0, 0,
                plane, 0, 0,
                1, 1,
                8193, 1);
    }

    @Test
    public void rejectsOversizedPlanePayloadBeforeCallingNative() {
        ByteBuffer plane = directPlane();

        NativeMethods.onCameraFrameData(1, 0L, 1,
                plane, 0, 64 * 1024 * 1024 + 1, 1,
                plane, 0, 0,
                plane, 0, 0,
                1, 1,
                1, 1);
    }

    @Test
    public void rejectsAFrameWhoseRgbaWouldBeOverBudgetBeforeCallingNative() {
        ByteBuffer plane = directPlane();

        // 4096 x 4097 is under the dimension cap; its RGBA is a row over 64 MiB.
        NativeMethods.onCameraFrameData(1, 0L, 1,
                plane, 0, 1, 4096,
                plane, 0, 0,
                plane, 0, 0,
                2048, 1,
                4096, 4097);
    }

    @Test
    public void rejectsAStrideThatIsNotPositiveBeforeCallingNative() {
        ByteBuffer plane = directPlane();

        NativeMethods.onCameraFrameData(1, 0L, 1,
                plane, 0, 1, 1,
                plane, 0, 1,
                plane, 0, 1,
                1, 0,
                1, 1);
    }
}
