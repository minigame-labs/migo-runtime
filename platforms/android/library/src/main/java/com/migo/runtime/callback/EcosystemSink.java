package com.migo.runtime.callback;

import java.util.Map;

/**
 * The channel an {@link EcosystemHandler} settles one request on.
 * <p>
 * Exactly one of these methods takes effect. The first call settles the
 * request; later calls do nothing.
 *
 * <h2>Threading</h2>
 * Every method is safe to call from any thread. Calls for a session that has
 * ended are ignored.
 */
public interface EcosystemSink {

    /**
     * Settle the request as successful.
     *
     * @param result the fields content reads off the API's result, as a tree of
     *               {@code Map}, {@code List}, {@code String}, {@code Number},
     *               {@code Boolean} and {@code null}; or null for none
     */
    void succeed(Map<String, ?> result);

    /**
     * Settle the request as failed.
     *
     * @param errCode the code the API defines for this failure, or -1 if none
     * @param errMsg  the message content receives verbatim as {@code errMsg},
     *                conventionally {@code "<api>:fail <reason>"}
     */
    void fail(int errCode, String errMsg);
}
