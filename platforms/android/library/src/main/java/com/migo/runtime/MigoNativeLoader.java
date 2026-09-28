package com.migo.runtime;

import android.content.Context;
import android.content.res.AssetManager;
import android.os.Build;
import android.util.Log;

import com.migo.runtime.internal.NativeArtifactExpectation;
import com.migo.runtime.internal.NativeArtifactManifests;
import com.migo.runtime.internal.NativeArtifactVerifier;
import com.migo.runtime.internal.NativeLinker;
import com.migo.runtime.internal.NativeLoadCoordinator;

import java.io.File;
import java.io.IOException;
import java.io.InputStream;

/**
 * Controls how the Migo engine binary enters the process.
 *
 * <p>By default nothing here needs calling: {@code libmigo.so} ships inside the
 * AAR, {@code System.loadLibrary} finds it, and this class stays out of the
 * way. It exists for hosts that would rather not carry ~17 MB of download and
 * ~45 MB of install for users who may never open a mini-game.
 *
 * <p>To take that path, depend on the {@code -nojni} AAR — the same build with
 * {@code jni/**} removed — and install a {@link NativeLibraryProvider}:
 *
 * <pre>{@code
 * // Application.onCreate(), before any other Migo call
 * MigoNativeLoader.setProvider(this, abi -> {
 *     File engine = new File(getNoBackupFilesDir(), abi + "/libmigo.so");
 *     return engine.isFile() ? engine : null;
 * });
 *
 * // When the user opens a mini-game and the engine is not on disk yet:
 * MigoNativeLoader.RequiredArtifact want = MigoNativeLoader.requiredArtifact(this);
 * // download want.abi's binary from wherever you host it, verify want.sha256,
 * // move it into place atomically, then start the game as usual.
 * }</pre>
 *
 * <p>The download itself is deliberately yours: on Google Play the only
 * compliant source is Play Feature Delivery, while other stores have no such
 * mechanism and expect you to host the file. A downloader baked in here would
 * be wrong for one of the two.
 */
public final class MigoNativeLoader {

    private static final String TAG = "MigoNativeLoader";
    private static final String LIBRARY_NAME = "migo";

    /** ABIs an engine slice is published for. */
    private static final String ABI_ARM64 = "arm64-v8a";
    private static final String ABI_X86_64 = "x86_64";

    private static final NativeLoadCoordinator COORDINATOR =
            new NativeLoadCoordinator(NativeLinker.SYSTEM, LIBRARY_NAME);

    private static volatile Context sContext;

    private MigoNativeLoader() {}

    /** Where the engine load stands. */
    public enum State {
        /** No load has succeeded yet. */
        NOT_LOADED,
        /** The engine has been loaded into the process. */
        LOADED,
        /** The last attempt failed. Another may be made — see {@link #lastError()}. */
        FAILED
    }

    /** What a delivered engine binary must be, for this device and this SDK build. */
    public static final class RequiredArtifact {
        /** The ABI to fetch, {@code arm64-v8a} or {@code x86_64}. */
        public final String abi;
        /** Lowercase hex SHA-256 the delivered {@code libmigo.so} must have. */
        public final String sha256;
        /** The slice's artifact identity, as published in the release manifest. */
        public final String artifactId;
        /** The SDK version this expectation belongs to. */
        public final String sdkVersion;

        RequiredArtifact(String abi, String sha256, String artifactId, String sdkVersion) {
            this.abi = abi;
            this.sha256 = sha256;
            this.artifactId = artifactId;
            this.sdkVersion = sdkVersion;
        }

        @Override
        public String toString() {
            return "RequiredArtifact{abi=" + abi + ", sha256=" + sha256
                    + ", artifactId=" + artifactId + ", sdkVersion=" + sdkVersion + "}";
        }
    }

    /**
     * Install the host's engine delivery hook.
     *
     * <p>Call before any other Migo API. Once the engine is loaded a provider
     * can no longer affect anything, so installing one then throws rather than
     * leaving you believing it took effect.
     *
     * @param context any context; the application context is retained
     * @param provider the hook, or {@code null} to return to loading from the APK
     * @throws IllegalStateException if the engine is already loaded
     */
    public static void setProvider(Context context, NativeLibraryProvider provider) {
        if (context == null) {
            throw new IllegalArgumentException("context is required to read the artifact manifest");
        }
        sContext = context.getApplicationContext();
        COORDINATOR.setProvider(provider);
    }

    /**
     * Verify a delivered engine binary now, on the thread you call from.
     *
     * <p>Optional, and it does not make the load safer: the engine is verified
     * when it is loaded whether or not you call this. What it changes is
     * <em>when you find out</em>. Without it, a truncated download or a mirror
     * still serving the previous release is discovered when a user opens a
     * game, as a launch failure; with it, your download code learns
     * immediately, while it still has the network connection and the user has
     * not asked for anything yet.
     *
     * <p>It also moves the check off the main thread. Verification is a SHA-256
     * over the whole binary -- 41 ms for a 45 MB release engine on a reference
     * Android device -- and without this it runs inside the first Migo call, which is on
     * the main thread while the user waits. Afterwards that call finds the
     * result already recorded and hashes nothing (3 ms).
     *
     * <p>Call it from the thread that finished the download, immediately after
     * the file is in its final place. Calling it again for an unchanged file is
     * cheap; calling it for a file that is later replaced is harmless, because
     * the recorded result does not outlive the bytes it describes.
     *
     * <pre>{@code
     * // on your download thread, after the atomic rename
     * String rejected = MigoNativeLoader.prepare(context, engine);
     * if (rejected != null) {
     *     engine.delete();          // fetch it again; do not hand it over
     *     reportToYourTelemetry(rejected);
     * }
     * }</pre>
     *
     * @param context any context; used to read this build's artifact manifest
     * @param engine  the file a {@link NativeLibraryProvider} would return
     * @return {@code null} when the file is what this SDK build expects, or a
     *         human-readable reason why it is not
     */
    public static String prepare(Context context, File engine) {
        Context resolved = context == null ? sContext : context;
        if (resolved == null) {
            return "no context to read the artifact manifest from; call setProvider first "
                    + "or pass one here";
        }
        sContext = resolved.getApplicationContext();
        NativeArtifactExpectation expectation = readExpectation(sContext, deviceAbi());
        NativeArtifactVerifier.Outcome outcome =
                NativeArtifactVerifier.verify(engine, expectation);
        if (outcome.accepted()) {
            return null;
        }
        Log.w(TAG, "prepare rejected the delivered engine binary: " + outcome.reason());
        return outcome.reason();
    }

    /** @return where the engine load stands. */
    public static State state() {
        switch (COORDINATOR.state()) {
            case LOADED:
                return State.LOADED;
            case FAILED:
                return State.FAILED;
            default:
                return State.NOT_LOADED;
        }
    }

    /** @return the last load failure, or {@code null}. Cleared by a successful load. */
    public static Throwable lastError() {
        return COORDINATOR.error();
    }

    /**
     * What this device needs, so the host can fetch and check it before
     * handing it over. Reads the manifest embedded in the AAR; does not load
     * the engine and does not touch the network.
     *
     * @return the expectation, or {@code null} if this build carries no
     *         artifact manifest (an unofficial or locally assembled AAR)
     */
    public static RequiredArtifact requiredArtifact(Context context) {
        String abi = deviceAbi();
        NativeArtifactExpectation expectation =
                readExpectation(context == null ? sContext : context, abi);
        if (expectation == null) {
            return null;
        }
        return new RequiredArtifact(
                expectation.abi(), expectation.sha256(), expectation.artifactId(),
                BuildInfo.VERSION);
    }

    /**
     * The ABI whose engine slice this device needs.
     *
     * <p>The first supported ABI wins, matching how the platform itself picks
     * which {@code jni/<abi>} directory to use. A device reporting only ABIs
     * Migo publishes no slice for gets {@code arm64-v8a}, so the failure is a
     * readable hash mismatch rather than a silent wrong-architecture load.
     */
    public static String deviceAbi() {
        String[] supported = Build.SUPPORTED_ABIS;
        if (supported != null) {
            for (int i = 0; i < supported.length; i++) {
                if (ABI_ARM64.equals(supported[i]) || ABI_X86_64.equals(supported[i])) {
                    return supported[i];
                }
            }
        }
        return ABI_ARM64;
    }

    // ==================== SDK-internal ====================

    /**
     * Load the engine if it is not loaded already.
     *
     * @return true when the engine has been loaded into the process
     */
    static boolean ensureLoaded() {
        boolean loaded = COORDINATOR.ensureLoaded(deviceAbi(), EXPECTATIONS);
        if (!loaded) {
            Throwable error = COORDINATOR.error();
            Log.e(TAG, "Failed to load native library: "
                    + (error == null ? "unknown" : error.getMessage()));
        }
        return loaded;
    }

    /** Where the loaded engine came from, for diagnostics. */
    static String loadedFrom() {
        return COORDINATOR.loadedFrom();
    }

    private static final NativeLoadCoordinator.ExpectationSource EXPECTATIONS =
            new NativeLoadCoordinator.ExpectationSource() {
                @Override
                public NativeArtifactExpectation expectationFor(String abi) {
                    return readExpectation(sContext, abi);
                }
            };

    private static NativeArtifactExpectation readExpectation(Context context, String abi) {
        if (context == null) {
            return null;
        }
        AssetManager assets = context.getAssets();
        if (assets == null) {
            return null;
        }
        InputStream in = null;
        try {
            in = assets.open(NativeArtifactManifests.sliceAssetPath(abi));
            return NativeArtifactManifests.parseSlice(in, abi);
        } catch (IOException e) {
            Log.w(TAG, "No usable artifact manifest for " + abi + ": " + e.getMessage());
            return null;
        } finally {
            if (in != null) {
                try {
                    in.close();
                } catch (IOException ignored) {
                    // The manifest is already parsed or already failed.
                }
            }
        }
    }

}
