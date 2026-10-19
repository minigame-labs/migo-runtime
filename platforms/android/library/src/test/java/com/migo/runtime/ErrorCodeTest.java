package com.migo.runtime;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public final class ErrorCodeTest {
    @Test
    public void inputSaturationIsStableAndRecoverable() {
        assertEquals(11, ErrorCode.NATIVE_INPUT_SATURATED);
        assertEquals("Input transport saturated",
                ErrorCode.getMessage(ErrorCode.NATIVE_INPUT_SATURATED));
        assertTrue(ErrorCode.isRecoverable(ErrorCode.NATIVE_INPUT_SATURATED));
    }

    /**
     * A render command the game got wrong does not end the game. It reached hosts with the
     * command's own code -- an invalid argument, a shader that did not compile -- which this
     * class read as non-recoverable, so a host that closes on those closed on a GL error.
     */
    @Test
    public void aFailedRenderCommandIsRecoverable() {
        assertEquals(430, ErrorCode.NATIVE_RENDER_COMMAND_FAILED);
        assertEquals("Render command failed",
                ErrorCode.getMessage(ErrorCode.NATIVE_RENDER_COMMAND_FAILED));
        assertTrue(ErrorCode.isRecoverable(ErrorCode.NATIVE_RENDER_COMMAND_FAILED));
    }
}
