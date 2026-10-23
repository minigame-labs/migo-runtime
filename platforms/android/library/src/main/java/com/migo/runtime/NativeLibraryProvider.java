package com.migo.runtime;

import java.io.File;

/**
 * Delivers the Migo engine binary when it is not packaged in the APK.
 *
 * <p>Install one with {@link MigoNativeLoader#setProvider} to keep
 * {@code libmigo.so} (~19 MB compressed, ~45 MB installed, per ABI) out of your
 * first-install download. Users who never open a mini-game never pay for it.
 *
 * <p>The SDK never downloads anything itself, because the compliant way to
 * obtain the binary depends on where you ship:
 *
 * <ul>
 *   <li><b>Google Play</b> — fetching executable code from anywhere but Play
 *       violates the Device and Network Abuse policy. Use Play Feature
 *       Delivery: put the engine in an on-demand module and return the path
 *       Play installed.</li>
 *   <li><b>Other stores</b> — Feature Delivery does not exist there; hosting
 *       the binary yourself and downloading it is the ordinary route.</li>
 * </ul>
 *
 * <p>Whatever the source, the file you return is verified against the artifact
 * manifest embedded in this SDK build before it is loaded, so a partial
 * download or a mirror serving the previous release fails at load with a
 * readable reason instead of crashing inside the engine.
 *
 * <h3>Contract</h3>
 * <ul>
 *   <li>{@link #resolve} is called on the thread that first needs the engine.
 *       Return promptly: return {@code null} when the binary is not on disk
 *       yet, rather than blocking on a download.</li>
 *   <li>Returning {@code null} is not fatal. The load fails, your code can
 *       fetch the binary, and the next attempt succeeds — no process restart.</li>
 *   <li>The returned file must stay readable for the life of the process.
 *       Replacing it after load has no effect; deleting it may crash the
 *       process.</li>
 * </ul>
 *
 * <pre>{@code
 * MigoNativeLoader.setProvider(context, abi -> {
 *     File engine = new File(context.getNoBackupFilesDir(), abi + "/libmigo.so");
 *     return engine.isFile() ? engine : null;   // null => "not yet"; go download it
 * });
 * }</pre>
 *
 * @see MigoNativeLoader#requiredArtifact(android.content.Context)
 */
public interface NativeLibraryProvider {

    /**
     * @param abi the device's primary ABI, {@code arm64-v8a} or {@code x86_64}
     * @return the engine binary for {@code abi}, or {@code null} if it is not
     *         available yet
     */
    File resolve(String abi);
}
