package com.migo.runtime.internal;

import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertNotSame;
import static org.junit.Assert.assertSame;

import com.migo.runtime.callback.PermissionSink;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.annotation.Config;

/**
 * A host reaches a session's standing decisions two ways -- the sink a request hands it, and
 * the one {@code GameSession#getPermissionSink} returns for decisions made outside a request --
 * and the public contract says they are the same sink. Two sinks would each be correct and
 * still disagree about nothing, since the decisions live in the gates behind them; what this
 * pins is the promise a host reads, so a second construction path cannot creep in.
 */
@RunWith(RobolectricTestRunner.class)
@Config(sdk = 35, manifest = Config.NONE)
public final class OneSinkPerSessionTest {
    @Test
    public void theSinkARequestIsHandedIsTheOneTheSessionGivesOut() {
        int sessionId = 31_001;
        RuntimeRegistry.register(new RuntimeContext(sessionId, null));
        try {
            PermissionSink[] handed = new PermissionSink[1];
            NativeExports.setPermissionHandler(sessionId, (requestId, scope, desc, sink) -> handed[0] = sink);
            NativeExports.permissionRequest(sessionId, "{\"requestId\":1,\"scope\":\"scope.camera\"}");

            assertNotNull("the handler was not asked", handed[0]);
            assertSame(NativeExports.permissionSink(sessionId), handed[0]);
            assertNotSame(NativeExports.permissionSink(sessionId), NativeExports.permissionSink(sessionId + 1));
        } finally {
            NativeExports.setPermissionHandler(sessionId, null);
            RuntimeRegistry.unregister(sessionId);
        }
    }
}
