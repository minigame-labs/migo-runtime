package com.migo.runtime.internal;

import com.migo.runtime.NativeLibraryProvider;

import java.io.File;

/**
 * Decides how the engine binary enters the process, and remembers what happened.
 * <p>
 * Two deliveries exist, and the SDK is deliberately neutral between them
 * because the choice is not the SDK's to make:
 * <ul>
 *   <li><b>Packaged</b> (default) -- {@code libmigo.so} sits in the APK's JNI
 *       directories and {@code System.loadLibrary} finds it. Nothing about an
 *       existing integration changes.</li>
 *   <li><b>Provided</b> -- the host installs a
 *       {@link com.migo.runtime.NativeLibraryProvider} and hands over a file it
 *       obtained however its store allows. On Google Play that must be Play
 *       Feature Delivery; elsewhere it is usually the host's own download. The
 *       SDK never fetches anything itself, so one interface serves both.</li>
 * </ul>
 *
 * <h3>Failure is not terminal</h3>
 * A failed attempt can be retried. This is what makes "user opened the game
 * before the download finished" recoverable without restarting the process:
 * the host resolves nothing, gets a diagnosable failure, finishes the
 * download, and the next attempt succeeds.
 *
 * <p>{@link NativeLibraryProvider} is used directly rather than mirrored here:
 * it is a public type but carries no android.* dependency, so importing it costs
 * this class nothing and saves an adapter that would exist only to re-declare one
 * method.
 *
 * @hide
 */
public final class NativeLoadCoordinator {

    /** Where the load stands. */
    public enum State {
        /** No attempt has succeeded yet. */
        NOT_LOADED,
        /** The engine has been loaded into the process. */
        LOADED,
        /** The last attempt failed; another may be made. */
        FAILED
    }

    /** Supplies the manifest expectation for an ABI, consulted only on the provided path. */
    public interface ExpectationSource {
        /** @return the expectation, or {@code null} when none can be read. */
        NativeArtifactExpectation expectationFor(String abi);
    }

    private final NativeLinker linker;
    private final String libraryName;

    private State state = State.NOT_LOADED;
    private Throwable error;
    private NativeLibraryProvider provider;
    private String loadedFrom;

    public NativeLoadCoordinator(NativeLinker linker, String libraryName) {
        if (linker == null) {
            throw new IllegalArgumentException("linker is required");
        }
        if (libraryName == null || libraryName.isEmpty()) {
            throw new IllegalArgumentException("libraryName is required");
        }
        this.linker = linker;
        this.libraryName = libraryName;
    }

    /**
     * Install the host's provider. Must happen before the engine is in the
     * process; afterwards there is nothing a provider could still influence,
     * and silently accepting it would leave the host believing it took effect.
     */
    public synchronized void setProvider(NativeLibraryProvider newProvider) {
        if (state == State.LOADED) {
            throw new IllegalStateException(
                    "the Migo engine is already loaded from " + loadedFrom
                            + "; install the provider before the first runtime call");
        }
        this.provider = newProvider;
    }

    public synchronized NativeLibraryProvider provider() {
        return provider;
    }

    public synchronized State state() {
        return state;
    }

    /** The last failure, or {@code null}. Cleared by a successful load. */
    public synchronized Throwable error() {
        return error;
    }

    /** Where the loaded engine came from, for diagnostics; {@code null} until loaded. */
    public synchronized String loadedFrom() {
        return loadedFrom;
    }

    /**
     * Load the engine if it is not loaded already.
     *
     * @param abi         the device's primary ABI, used only on the provided path
     * @param expectations consulted only on the provided path, so the default
     *                     path never pays for reading the manifest
     * @return true when the engine has been loaded into the process
     */
    public synchronized boolean ensureLoaded(String abi, ExpectationSource expectations) {
        if (state == State.LOADED) {
            return true;
        }
        NativeLibraryProvider active = provider;
        try {
            if (active == null) {
                try {
                    linker.loadLibrary(libraryName);
                } catch (UnsatisfiedLinkError e) {
                    // The most likely integration mistake reaches here as
                    // dlopen's "library not found", which names neither cause.
                    // Both causes are worth naming: the -nojni AAR ships no
                    // engine at all, and an abiFilters list that excludes this
                    // device produces the identical symptom.
                    throw new UnsatisfiedLinkError(e.getMessage()
                            + " -- no engine binary for " + abi + " in the application package. "
                            + "If you depend on the -nojni AAR, install a NativeLibraryProvider "
                            + "via MigoNativeLoader.setProvider; otherwise check that abiFilters "
                            + "includes " + abi);
                }
                succeed("the application package");
                return true;
            }
            File delivered = active.resolve(abi);
            if (delivered == null) {
                fail(new UnsatisfiedLinkError(
                        "the host's NativeLibraryProvider has no engine binary for " + abi
                                + " yet; deliver it and retry"));
                return false;
            }
            NativeArtifactExpectation expectation =
                    expectations == null ? null : expectations.expectationFor(abi);
            NativeArtifactVerifier.Outcome outcome =
                    NativeArtifactVerifier.verify(delivered, expectation);
            if (!outcome.accepted()) {
                fail(new UnsatisfiedLinkError(
                        "rejected the delivered engine binary: " + outcome.reason()));
                return false;
            }
            linker.load(delivered.getAbsolutePath());
            succeed(delivered.getAbsolutePath());
            return true;
        } catch (UnsatisfiedLinkError e) {
            fail(e);
            return false;
        } catch (RuntimeException e) {
            // A provider is host code: a throwing resolve() must surface as a
            // load failure the host can read, not as an exception escaping from
            // whatever SDK call happened to trigger the first load.
            fail(e);
            return false;
        }
    }

    private void succeed(String source) {
        state = State.LOADED;
        error = null;
        loadedFrom = source;
    }

    private void fail(Throwable cause) {
        state = State.FAILED;
        error = cause;
        loadedFrom = null;
    }
}
