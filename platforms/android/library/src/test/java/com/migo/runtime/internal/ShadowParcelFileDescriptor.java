package com.migo.runtime.internal;

import android.os.ParcelFileDescriptor;

import java.io.IOException;

import org.robolectric.annotation.Implementation;
import org.robolectric.annotation.Implements;

/** Injects encoded-pipe setup failure while retaining Robolectric's real pipe behavior. */
@Implements(ParcelFileDescriptor.class)
public final class ShadowParcelFileDescriptor extends org.robolectric.shadows.ShadowParcelFileDescriptor {
    static boolean createPipeFailure;

    @Implementation
    public static ParcelFileDescriptor[] createPipe() throws IOException {
        if (createPipeFailure) throw new IOException("pipe setup failed");
        return org.robolectric.shadows.ShadowParcelFileDescriptor.createPipe();
    }

    public static void reset() {
        createPipeFailure = false;
    }
}
