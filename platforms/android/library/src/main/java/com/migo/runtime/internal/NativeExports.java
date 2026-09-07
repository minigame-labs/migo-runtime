package com.migo.runtime.internal;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.net.Uri;
import android.provider.Settings;

import com.migo.runtime.internal.platform.AdpfManager;
import com.migo.runtime.internal.platform.BatteryInfo;
import com.migo.runtime.internal.platform.Clipboard;
import com.migo.runtime.internal.platform.DeviceInfo;
import com.migo.runtime.internal.platform.InteractionUI;
import com.migo.runtime.internal.platform.DisplayCompat;
import com.migo.runtime.internal.platform.LocationProvider;
import com.migo.runtime.internal.platform.Permissions;
import com.migo.runtime.internal.platform.ScreenBrightness;
import com.migo.runtime.internal.platform.SystemSettings;
import com.migo.runtime.internal.platform.Vibrator;
import com.migo.runtime.callback.AdEventSink;
import com.migo.runtime.callback.AdHandler;
import com.migo.runtime.callback.AuthHandler;
import com.migo.runtime.callback.GameLogHandler;
import com.migo.runtime.callback.NavigationHandler;
import com.migo.runtime.callback.PaymentHandler;
import com.migo.runtime.callback.PermissionHandler;
import com.migo.runtime.callback.PermissionSink;
import com.migo.runtime.callback.SettingHandler;
import com.migo.runtime.callback.ShareHandler;
import com.migo.runtime.callback.SubpackageHandler;

import com.migo.runtime.GameSession;
import com.migo.runtime.SessionState;
import com.migo.runtime.BuildConfig;
import com.migo.runtime.ErrorCode;

import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.File;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.concurrent.ConcurrentHashMap;
import java.util.function.BooleanSupplier;

/**
 * Static methods exposed to native code via JNI.
 * <p>
 * These methods are called from Rust/native code to access Android platform features.
 * Method signatures must match those registered in registration.rs.
 * <p>
 * Domain-specific managers are delegated to:
 * - {@link SensorExports}: device sensors, screen capture
 * - {@link NetworkExports}: network monitoring
 * - {@link MediaExports}: recorder, camera, image API, video
 * - {@link BluetoothExports}: Bluetooth and BLE
 * - {@link InputExports}: keyboard, scan code
 *
 * @hide
 */
public final class NativeExports {
    private static final String TAG = "NativeExports";

    private static final int BLUETOOTH_SETTING_REQUEST_CODE = 10001;
    private static final int APP_AUTHORIZE_SETTING_REQUEST_CODE = 10002;

    /** Per-session error callbacks (registered by GameSession). */
    private static final ConcurrentHashMap<Integer, NativeErrorCallback> sErrorCallbacks =
            new ConcurrentHashMap<>();

    /** Per-session GameSession references for lifecycle callbacks. */
    private static final ConcurrentHashMap<Integer, GameSession> sSessions =
            new ConcurrentHashMap<>();

    /**
     * A surface loss reported before its session was registered, kept until it can be
     * delivered.
     *
     * <p>{@code init} spawns the render thread, and only then does the {@code GameSession}
     * constructor register. A session started <em>with</em> a Surface can therefore have
     * that Surface refused inside that window — and dropping the report would leave the
     * app with no signal that it must attach another. This is the one callback where that
     * matters: it is the only way an app learns that.
     *
     * <p>The window is real only for {@code createSession(Context, ...)}, which is
     * documented for Service use and does not require the main thread; the
     * {@code Activity} overload does, so main-thread serialization already puts
     * registration ahead of any posted report.
     *
     * <p>The first is kept rather than the last, as at the C boundary: a second loss in
     * that window is for a Surface whose predecessor's loss has not been delivered yet, so
     * replaying the newer one would skip the one the app needs.
     *
     * <p>What this does <em>not</em> close: {@code setListener} is a separate call, so a
     * replay can still arrive before a listener exists. That is not specific to this
     * callback — {@code notifyGameReady} and {@code notifyError} drop on a null listener
     * too — and inventing retention for one of them would make the set inconsistent. This
     * takes delivery from impossible to likely, which is the part that was broken.
     */
    private static final ConcurrentHashMap<Integer, long[]> sPendingSurfaceLoss =
            new ConcurrentHashMap<>();

    /** Per-session auth handlers set via GameSession API. */
    private static final ConcurrentHashMap<Integer, AuthHandler> sAuthHandlers =
            new ConcurrentHashMap<>();

    /** Per-session permission handlers set via GameSession API. */
    private static final ConcurrentHashMap<Integer, PermissionHandler> sPermissionHandlers =
            new ConcurrentHashMap<>();

    private static final PermissionOperationGate sPermissionOperations =
            new PermissionOperationGate();
    private static final TerminalCloseQueue<GameSession> sTerminalCloses =
            new TerminalCloseQueue<>();

    private static final PermissionRevocation.ResourceTeardown sPermissionResources =
            new PermissionRevocation.ResourceTeardown() {
                @Override
                public void destroyCamera(int sessionId) {
                    if (BuildConfig.MIGO_API_MEDIA) {
                        MediaExports.destroyCameraManagers(sessionId);
                    }
                }

                @Override
                public void destroyRecorder(int sessionId) {
                    if (BuildConfig.MIGO_API_MEDIA) {
                        MediaExports.destroyRecorderManager(sessionId);
                    }
                }

                @Override
                public void destroyBluetooth(int sessionId) {
                    if (BuildConfig.MIGO_API_CONNECTIVITY) {
                        BluetoothExports.destroyBluetoothManager(sessionId);
                    }
                }
            };

    /** Per-session ad handlers set via GameSession API. */
    private static final ConcurrentHashMap<Integer, AdHandler> sAdHandlers =
            new ConcurrentHashMap<>();

    /** Per-session game log handlers set via GameSession API. */
    private static final ConcurrentHashMap<Integer, GameLogHandler> sGameLogHandlers =
            new ConcurrentHashMap<>();

    /** Per-session subpackage handlers set via GameSession API. */
    private static final ConcurrentHashMap<Integer, SubpackageHandler> sSubpackageHandlers =
            new ConcurrentHashMap<>();

    /** Per-session setting handlers set via GameSession API. */
    private static final ConcurrentHashMap<Integer, SettingHandler> sSettingHandlers =
            new ConcurrentHashMap<>();

    /** Per-session share handlers set via GameSession API. */
    private static final ConcurrentHashMap<Integer, ShareHandler> sShareHandlers =
            new ConcurrentHashMap<>();

    /** Per-session navigation handlers set via GameSession API. */
    private static final ConcurrentHashMap<Integer, NavigationHandler> sNavigationHandlers =
            new ConcurrentHashMap<>();

    /** Per-session payment handlers set via GameSession API. */
    private static final ConcurrentHashMap<Integer, PaymentHandler> sPaymentHandlers =
            new ConcurrentHashMap<>();

    /** Per-session message handlers set via GameSession.setMessageHandler(). */
    private static final ConcurrentHashMap<Integer, GameSession.MessageHandler> sMessageHandlers =
            new ConcurrentHashMap<>();

    /** Handler for dispatching callbacks to the main thread. */
    private static final Handler sMainHandler = new Handler(Looper.getMainLooper());

    /** R1 hot-path message code; uses pooled Message objects instead of one Runnable allocation/frame. */
    private static final int MSG_REQUEST_VSYNC = 1;

    /** Dedicated frame-arm handler. The callback object is allocated once at class initialization. */
    private static final Handler sVsyncHandler = new Handler(Looper.getMainLooper(), message -> {
        if (message.what != MSG_REQUEST_VSYNC) {
            return false;
        }
        GameSession session = sSessions.get(message.arg1);
        if (session != null) {
            session.requestVsyncFrame();
        }
        return true;
    });

    private NativeExports() {}

    // ==================== Error Notification (from native) ====================

    /**
     * Callback interface for native engine errors and recoverable pressure.
     * <p>
     * Implemented by the session owner (e.g. {@code GameSession}) to receive
     * fatal error notifications from the Rust engine.
     *
     * @hide
     */
    public interface NativeErrorCallback {
        /**
         * Called when a native engine error or recoverable pressure event occurs.
         * <p>
         * Always called on the <b>main thread</b>.
         *
         * @param errorCode Native error code (see {@code ErrorCode.NATIVE_*} constants)
         * @param message   Human-readable error message
         * @param detail    Detailed information (stack trace, etc.), may be empty
         */
        void onNativeError(int errorCode, String message, String detail);

        /**
         * Called when the mini program exits.
         * <p>
         * Always called on the <b>main thread</b>.
         */
        void onExit();
    }

    /**
     * Register an error callback for a session.
     * <p>
     * Call during session creation. Only one callback per session is supported;
     * a subsequent registration replaces the previous one.
     *
     * @param sessionId The session ID
     * @param callback  The callback to receive error notifications
     * @hide
     */
    public static void registerErrorCallback(int sessionId, NativeErrorCallback callback) {
        if (callback != null) {
            sErrorCallbacks.put(sessionId, callback);
        }
    }

    /**
     * Unregister the error callback for a session.
     * <p>
     * Call during session shutdown.
     *
     * @param sessionId The session ID
     * @hide
     */
    public static void unregisterErrorCallback(int sessionId) {
        sErrorCallbacks.remove(sessionId);
    }

    /**
     * Register a GameSession for lifecycle callbacks from native.
     * @hide
     */
    public static void registerSession(int sessionId, GameSession session) {
        if (session != null) {
            PermissionOperationGate.Admission admission = sPermissionOperations.admit(sessionId);
            if (admission != PermissionOperationGate.Admission.ADMITTED) {
                // The gate refuses two different things and they mean different things to
                // a host, so the message says which. A retired id was closed and can
                // never be granted anything again; a live one is a duplicate
                // registration. The single message this used to throw named the closing
                // case for both, which told a host the opposite of what happened in the
                // other half.
                //
                // Both are fatal here, unlike on the native side, where a live-id refusal
                // is tolerated because `HostCommand::Restart` rebuilds device services for
                // the same live id. Nothing re-registers on this side: this is the
                // `GameSession` constructor's only call, and `restart()` goes straight to
                // native without rebuilding the wrapper.
                throw new IllegalStateException(
                        admission == PermissionOperationGate.Admission.RETIRED
                                ? "session " + sessionId + " was already closed; its id"
                                        + " must not be reused, and every permission check"
                                        + " for it is denied"
                                : "session " + sessionId + " is already registered;"
                                        + " concurrently live sessions must have distinct"
                                        + " ids");
            }
            sSessions.put(sessionId, session);
            // Ordered after the gate's admission so the two agree on which ids
            // are live: the gate is what rejects a reused or duplicate id, and
            // registering a generation for one it refused would leave a session
            // numbered here that exists nowhere else.
            RuntimeGenerationBoundary.registerSession(sessionId);
            // A loss the renderer reported while this session was still being
            // constructed. Posted rather than delivered inline: this runs on the
            // constructor's thread, which `MigoRuntime.createSession` does not require to
            // be the main one, and the callback contract is that it is.
            long[] pending = sPendingSurfaceLoss.remove(sessionId);
            if (pending != null) {
                sMainHandler.post(() -> {
                    GameSession live = sSessions.get(sessionId);
                    if (live != null) {
                        live.notifySurfaceLost(pending[0], (int) pending[1]);
                    }
                });
            }
        }
    }

    /**
     * The runtime at {@code retired} is being replaced by {@code next}.
     *
     * <p>Called by the engine as it drops the old isolate. Between this and
     * {@link #completeRuntimeRestart} nothing can be acquired for this session:
     * there is no live runtime for a new manager to belong to.
     *
     * @hide
     */
    public static void beginRuntimeRestart(int sessionId, long retired, long next) {
        // The boundary closes first, so nothing acquired during the teardown can
        // be handed the generation that is leaving. A teardown that then throws
        // leaves the session RESTARTING, which the completion reopens because it
        // is authoritative rather than matched against a remembered candidate.
        RuntimeGenerationBoundary.beginRestart(sessionId, retired, next);
        destroyRuntimeScopedManagers(sessionId);
    }

    /**
     * Tear down the per-session objects the runtime being replaced created.
     *
     * <p>Without this a restart leaves every listener the retired isolate
     * registered still registered: an accelerometer still sampling, a screenshot
     * observer still querying MediaStore, a keyboard still on screen — owned by
     * no runtime, reporting to nothing, until the session ends. The generation
     * fence makes those events harmless; it does not stop them costing battery
     * and memory.
     *
     * <p><b>What decides whether a group can be swept is its own teardown, not
     * whether its producer is fenced.</b> Destroying a manager can report: the
     * keyboard's emits an {@code onKeyboardComplete}, and a camera's emits a
     * stop. Those land on the queue while {@code on_restart} is still running on
     * the engine thread, so they are dispatched to the runtime that replaces this
     * one — as if it had produced them. A fenced producer stamps the retired
     * generation and the engine drops them; an unfenced one would have this sweep
     * inject exactly the cross-talk the fence exists to remove.
     *
     * <p>So a group qualifies if its teardown reports nothing, or if what it
     * reports is fenced. Sensors and input are here on the second count.
     * {@code NetworkMonitor} is here on the first: it unregisters a
     * {@link android.net.ConnectivityManager} callback and says nothing, and it
     * is not fenced *by design* — a network status is a current fact about the
     * device, like a screen rotation, so a replacement isolate needs it as much
     * as the retired one did. Media reports a {@code stop} as it releases the
     * camera and the microphone, and is here because those reports are fenced
     * now. Bluetooth still reports unfenced, so it waits for its tokens.
     *
     * <p>Handlers the embedder registered ({@code AdHandler}, {@code AuthHandler},
     * the message and permission sinks) are deliberately not here: they belong to
     * the session, not to the isolate, and the app registered them once.
     *
     * @param sessionId The session ID
     */
    private static void destroyRuntimeScopedManagers(int sessionId) {
        ResourceCleanup.runAll(
                () -> {
                    if (BuildConfig.MIGO_API_SENSORS) SensorExports.destroyAll(sessionId);
                },
                () -> {
                    if (BuildConfig.MIGO_API_SENSORS) NetworkExports.destroyAll(sessionId);
                },
                // First among all of these: a camera or a microphone held by no
                // runtime keeps the OS privacy indicator lit and keeps every
                // other app off the device for the rest of the session.
                () -> {
                    if (BuildConfig.MIGO_API_MEDIA) MediaExports.destroyAll(sessionId);
                },
                () -> InputExports.destroyAll(sessionId),
                () -> InteractionUI.destroy(sessionId));
    }

    /**
     * The runtime at {@code next} is live.
     *
     * <p>Every object created from here belongs to it, and every token issued
     * before the matching {@link #beginRuntimeRestart} is now stale.
     *
     * @hide
     */
    public static void completeRuntimeRestart(int sessionId, long next) {
        RuntimeGenerationBoundary.completeRestart(sessionId, next);
    }

    /**
     * Unregister a GameSession.
     * @hide
     */
    public static void unregisterSession(int sessionId) {
        sSessions.remove(sessionId);
        // A loss retained for a session that never registered has nobody left to tell,
        // and leaving it here would keep one entry per such session for the process's
        // life -- and hand it to whoever next used the id if ids were ever reused.
        sPendingSurfaceLoss.remove(sessionId);
        // Every token this session issued becomes stale rather than current by
        // default, so anything still holding one stops reporting.
        RuntimeGenerationBoundary.unregisterSession(sessionId);
    }

    /** Closes permission operations and retries any retained deferred cancellations. */
    public static void closePermissionOperations(int sessionId) {
        PermissionOperationGate.Result result = sPermissionOperations.close(sessionId);
        if (result.failure() != null) throw result.failure();
    }

    /**
     * Returns whether power-sensitive platform resources must remain inactive.
     * Missing, not-yet-running, paused, and destroyed sessions fail closed.
     *
     * @hide
     */
    public static boolean isSessionResourceSuspended(int sessionId) {
        GameSession session = sSessions.get(sessionId);
        return session == null || session.getState() != SessionState.RUNNING;
    }

    /** Returns true once a session can no longer own newly-created managers. */
    public static boolean isSessionTerminated(int sessionId) {
        GameSession session = sSessions.get(sessionId);
        return session == null || session.getState() == SessionState.DESTROYED;
    }

    /** Runs a callback admitted by the session's current standing scope. */
    public static boolean runIfPermissionGranted(
            int sessionId,
            String scope,
            BooleanSupplier callback) {
        return sPermissionOperations.runIfGranted(sessionId, scope, callback);
    }

    /** Suspend sensors, BLE scans, camera capture, and video for OnHide. */
    public static void suspendPowerSensitiveManagers(int sessionId) {
        if (BuildConfig.MIGO_API_SENSORS) {
            SensorExports.suspendPowerSensitiveManagers(sessionId);
        }
        if (BuildConfig.MIGO_API_CONNECTIVITY) {
            BluetoothExports.suspendPowerSensitiveManagers(sessionId);
        }
        if (BuildConfig.MIGO_API_MEDIA) {
            MediaExports.suspendPowerSensitiveManagers(sessionId);
        }
    }

    /** Restore only platform resources still requested when the session runs. */
    public static void resumePowerSensitiveManagers(int sessionId) {
        if (isSessionResourceSuspended(sessionId)) return;
        if (BuildConfig.MIGO_API_MEDIA) {
            MediaExports.resumePowerSensitiveManagers(sessionId);
        }
        if (BuildConfig.MIGO_API_CONNECTIVITY) {
            BluetoothExports.resumePowerSensitiveManagers(sessionId);
        }
        if (BuildConfig.MIGO_API_SENSORS) {
            SensorExports.resumePowerSensitiveManagers(sessionId);
        }
    }

    /**
     * Called from native code (Rust) when the game module has been loaded.
     * <p>
     * JNI signature: {@code (I)V}
     *
     * @param hostId Session/host ID
     */
    public static void onGameReady(int hostId) {
        sMainHandler.post(() -> {
            GameSession session = sSessions.get(hostId);
            if (session != null) {
                session.notifyGameReady();
            }
        });
    }

    /**
     * Called from native code (Rust render/host thread) to request exactly one
     * Choreographer frame callback (R1 demand-driven vsync).
     * <p>
     * Hops to the main thread and rechecks the live {@link GameSession} before
     * touching the Choreographer, so a call that races session teardown is a
     * harmless no-op. The scheduler coalesces duplicate requests.
     * <p>
     * JNI signature: {@code (I)V}
     *
     * @param hostId Session/host ID
     */
    public static void requestVsync(int hostId) {
        sVsyncHandler.obtainMessage(MSG_REQUEST_VSYNC, hostId, 0).sendToTarget();
    }

    /**
     * Called from native code (Rust) for engine errors and recoverable pressure.
     * <p>
     * This method is invoked from native threads (host thread, watchdog thread, etc.)
     * and dispatches the error to the registered callback on the <b>main thread</b>.
     * <p>
     * JNI signature: {@code (IILjava/lang/String;Ljava/lang/String;)V}
     *
     * @param hostId    Session/host ID
     * @param errorCode Native error code:
     *                  11 = InputSaturated,
     *                  203 = OutOfMemory, 204 = JsExecutionTimeout,
     *                  205 = HostPanic, 206 = ANR,
     *                  207 = CodeSignatureInvalid, 208 = CodeIntegrityFailed
     * @param message   Human-readable error message
     * @param detail    Detailed error information (stack trace, context)
     */
    public static void onError(int hostId, int errorCode, String message, String detail) {
        NativeErrorCallback callback = sErrorCallbacks.get(hostId);
        if (callback != null) {
            if (errorCode == ErrorCode.ERR_CLEANUP_FAILED) {
                sMainHandler.post(() -> callback.onNativeError(
                        errorCode,
                        message != null ? message : "Permission cleanup failed",
                        detail != null ? detail : ""));
                return;
            }
            // Dispatch to main thread — native calls may come from any thread
            sMainHandler.post(() -> {
                // Re-check: session may have been destroyed between post and dispatch
                NativeErrorCallback cb = sErrorCallbacks.get(hostId);
                if (cb != null) {
                    cb.onNativeError(errorCode,
                            message != null ? message : "Unknown native error",
                            detail != null ? detail : "");
                }
            });
        }
    }

    /**
     * Called from native code (Rust) when the mini program exits.
     * <p>
     * JNI signature: {@code (I)V}
     *
     * @param hostId    Session/host ID
     */
    public static void onExit(int hostId) {
        NativeErrorCallback callback = sErrorCallbacks.get(hostId);
        if (callback != null) {
            // Dispatch to main thread
            sMainHandler.post(() -> {
                NativeErrorCallback cb = sErrorCallbacks.get(hostId);
                if (cb != null) {
                    cb.onExit();
                }
            });
        }
    }

    /**
     * Called from native code (Rust) when a live Surface was retired after the engine
     * failed to present to it.
     * <p>
     * The opposite direction from {@link GameSession#onSurfaceDestroyed()}: that one is
     * the host taking its Surface back, this one is the engine reporting that the
     * Surface the host still believes in is gone. Nothing else carries it — the render
     * worker stays alive, so no channel closes and no reply arrives.
     * <p>
     * JNI signature: {@code (IJI)V}
     *
     * @param hostId     Session/host ID
     * @param generation The host-facing Surface generation that was lost
     * @param reason     0 unknown, 1 host destroyed, 2 device lost, 3 platform error
     */
    public static void onSurfaceLost(int hostId, long generation, int reason) {
        sMainHandler.post(() -> {
            GameSession session = sSessions.get(hostId);
            if (session != null) {
                session.notifySurfaceLost(generation, reason);
            } else if (!sPermissionOperations.isRetired(hostId)) {
                // Absent from the live map means either "has not registered yet" or
                // "already closed", and only the first is worth keeping: nothing will
                // ever register a closed id again, so an entry made for one would sit
                // here for the life of the process. `unregisterSession` removing the
                // entry does not cover this -- a report posted before the close can run
                // after it and put a new one back.
                sPendingSurfaceLoss.putIfAbsent(hostId, new long[] {generation, reason});
            }
        });
    }

    // ==================== Image Decoding ====================

    /**
     * Decode image bytes to RGBA using Android's BitmapFactory.
     * Returns [width_le32, height_le32, RGBA_bytes...] or null on failure.
     *
     * <p>ARGB_8888 (backed by Skia kRGBA_8888) stores bytes as R,G,B,A from low
     * to high address on little-endian devices (all Android).
     * {@code copyPixelsToBuffer} writes these bytes in memory order, so the
     * output is already in RGBA byte order — no swizzle needed.
     *
     * @param imageData Raw image file bytes (JPEG, PNG, BMP, etc.)
     * @return Packed byte array: 8-byte header (width + height as little-endian int32) + RGBA pixels, or null
     */
    public static byte[] decodeImageRgba(byte[] imageData) {
        if (imageData == null || imageData.length == 0) return null;

        // Fail fast when Java heap has less than 32 MB headroom.
        // Each decode needs ~2x(w*h*4) temporarily (Bitmap + output buffer),
        // attempting it under memory pressure just triggers GC storms or OOM.
        Runtime rt = Runtime.getRuntime();
        long used = rt.totalMemory() - rt.freeMemory();
        long free = rt.maxMemory() - used;
        if (free < 32L * 1024 * 1024) {
            return null;
        }

        // Hard image-bomb ceiling, mirroring the Rust MAX_IMAGE_PIXELS guard in
        // io/fast_image_decoder.rs: 64 Mpx = 8192x8192 = 256 MiB RGBA. A tiny
        // compressed file can declare huge dimensions; probing bounds first lets
        // us reject before allocating a multi-hundred-MB bitmap.
        final long MAX_IMAGE_PIXELS = 8192L * 8192L;

        Bitmap bitmap = null;
        try {
            BitmapFactory.Options opts = new BitmapFactory.Options();

            // Pass 1 -- bounds only (no pixel allocation). Read the declared
            // dimensions and reject an oversized / undecodable image up front,
            // before committing the full ARGB_8888 buffer.
            opts.inJustDecodeBounds = true;
            BitmapFactory.decodeByteArray(imageData, 0, imageData.length, opts);
            long probedPixels = (long) opts.outWidth * (long) opts.outHeight;
            if (opts.outWidth <= 0 || opts.outHeight <= 0 || probedPixels > MAX_IMAGE_PIXELS) {
                // Undecodable header or exceeds the pixel ceiling: refuse rather
                // than risk an OOM on the full decode.
                return null;
            }

            // Dynamic memory budget: the decode needs ~pixels*4 for the
            // ARGB_8888 bitmap plus pixels*4 for the exported byte[] below
            // (= pixels*8), plus slack for GC headroom. A large-but-under-cap
            // image (e.g. 8192x8192 = 256 MiB bitmap + 256 MiB output) can still
            // OOM / GC-storm a low-memory device, so refuse when the free heap
            // can't comfortably hold it. This complements the fixed 32 MiB
            // headroom gate above with a size-proportional one.
            long needBytes = probedPixels * 8L + (16L * 1024 * 1024);
            Runtime rt2 = Runtime.getRuntime();
            long free2 = rt2.maxMemory() - (rt2.totalMemory() - rt2.freeMemory());
            if (free2 < needBytes) {
                return null;
            }

            // Pass 2 -- real decode.
            opts.inJustDecodeBounds = false;
            opts.inPreferredConfig = Bitmap.Config.ARGB_8888;
            // Decode as non-premultiplied so RGB channels are unmodified.
            // The GL pipeline handles alpha blending; premultiplied data
            // would corrupt colours when used with straight-alpha blending.
            opts.inPremultiplied = false;

            bitmap = BitmapFactory.decodeByteArray(imageData, 0, imageData.length, opts);
            if (bitmap == null) return null;

            // Ensure ARGB_8888 config for consistent pixel format
            if (bitmap.getConfig() != Bitmap.Config.ARGB_8888) {
                Bitmap converted = bitmap.copy(Bitmap.Config.ARGB_8888, false);
                bitmap.recycle();
                bitmap = converted;
                if (bitmap == null) return null;
            }

            int w = bitmap.getWidth();
            int h = bitmap.getHeight();
            long pixelCountLong = (long) w * (long) h;
            if (pixelCountLong > Integer.MAX_VALUE / 4) {
                bitmap.recycle();
                return null;  // Image too large
            }
            int pixelCount = (int) pixelCountLong;

            // Single allocation: 8-byte header + pixel data (eliminates the
            // previous redundant pixelBuf allocation).
            ByteBuffer buf = ByteBuffer.allocate(8 + pixelCount * 4);
            buf.order(ByteOrder.LITTLE_ENDIAN);
            buf.putInt(w);
            buf.putInt(h);

            // copyPixelsToBuffer writes raw pixel bytes in native memory order.
            // Android's ARGB_8888 (backed by Skia kRGBA_8888) stores bytes as
            // R, G, B, A from low to high address — already RGBA byte order.
            // No swizzle needed.
            bitmap.copyPixelsToBuffer(buf);
            bitmap.recycle();
            bitmap = null;

            return buf.array();
        } catch (OutOfMemoryError e) {
            if (bitmap != null && !bitmap.isRecycled()) {
                bitmap.recycle();
            }
            return null;
        }
    }

    /**
     * Zero-copy {@code HardwareBuffer} ("AHB") image-decode hook.
     *
     * <p><b>Disabled — always returns {@code null}.</b> The engine's
     * minimum supported API level is 26, but the AHB fast path relied
     * on {@link android.graphics.ImageDecoder} (API 28) and
     * {@code Bitmap.getHardwareBuffer()} (API <b>31</b>). The old
     * {@code SDK_INT >= R} (API 30) gate was itself wrong: an API 30
     * device passed the gate and then invoked the API 31
     * {@code getHardwareBuffer()}, which raises
     * {@code NoSuchMethodError}/{@code VerifyError} (neither caught by
     * the former decoder's {@code Exception|OutOfMemoryError} handler).
     *
     * <p>To keep the whole library free of framework APIs above the
     * API 26 floor, this hook now returns {@code null} unconditionally,
     * so the native caller ({@code decode_image_ahb_jni}) falls back to
     * the API-26-safe {@link #decodeImageRgba}. The JNI layer resolves
     * this method by name, so the {@code decodeImageAhb([B)[B}
     * signature is preserved.
     *
     * <p>A genuine API-26 zero-copy path (Rust/Skia decode +
     * {@code AHardwareBuffer_allocate} through the NDK, available since
     * API 26) can replace this stub later; until then RGBA decode is
     * the only path.
     */
    public static byte[] decodeImageAhb(byte[] imageData) {
        // AHB fast path intentionally disabled: it required framework
        // APIs above the API 26 floor (ImageDecoder/API28,
        // Bitmap.getHardwareBuffer/API31). Returning null routes
        // decoding through the API-26-safe RGBA fallback in the caller.
        return null;
    }

    // ==================== File System ====================

    // Zip-bomb defense budget. MUST stay in sync with the Rust
    // `ExtractBudget::DEFAULT` in engine/crates/io/zip_extract.rs so
    // the Android (java.util.zip) unzip path enforces the same limits
    // as the desktop Rust path — otherwise a malicious/oversized zip
    // that Rust rejects would be extracted unbounded on Android,
    // exhausting storage or CPU.
    private static final int UNZIP_MAX_ENTRIES = 20_000;
    private static final long UNZIP_MAX_TOTAL_UNCOMPRESSED = 256L * 1024 * 1024;
    private static final long UNZIP_MAX_ENTRY_UNCOMPRESSED = 100L * 1024 * 1024;
    private static final long UNZIP_MAX_COMPRESSION_RATIO = 200L;

    /**
     * Extract a zip file to target directory using Android's built-in java.util.zip.
     * <p>
     * Includes path traversal protection (zip slip prevention) and a
     * zip-bomb resource budget (entry count, per-entry size, total
     * uncompressed size, compression ratio) mirroring the Rust
     * {@code ExtractBudget::DEFAULT}. Because {@code ZipInputStream} does
     * not reliably expose sizes from the local header (they can be -1
     * for streamed entries), the byte-size limits are enforced against
     * the bytes actually written while streaming, not just the
     * advertised header sizes.
     *
     * @param zipFilePath Path to the zip file
     * @param targetPath  Destination directory
     * @return Number of files extracted on success, or error message prefixed with "ERR:"
     */
    public static String unzipFile(String zipFilePath, String targetPath) {
        if (zipFilePath == null || targetPath == null) {
            return "ERR:unzip:fail invalid arguments";
        }

        File zipFile = new File(zipFilePath);
        if (!zipFile.exists()) {
            return "ERR:unzip:fail file not found: " + zipFilePath;
        }

        File destDir = new File(targetPath);
        if (!destDir.exists() && !destDir.mkdirs()) {
            return "ERR:unzip:fail cannot create destination directory";
        }

        String canonicalDest;
        try {
            canonicalDest = destDir.getCanonicalPath();
        } catch (java.io.IOException e) {
            return "ERR:unzip:fail cannot resolve destination path";
        }

        int fileCount = 0;
        int entryCount = 0;
        long totalWritten = 0;
        try (java.util.zip.ZipInputStream zis = new java.util.zip.ZipInputStream(
                new java.io.BufferedInputStream(new java.io.FileInputStream(zipFile), 65536))) {

            java.util.zip.ZipEntry entry;
            byte[] buffer = new byte[8192];

            while ((entry = zis.getNextEntry()) != null) {
                // Budget: entry count (files + directories), rejects inode bombs.
                entryCount++;
                if (entryCount > UNZIP_MAX_ENTRIES) {
                    // NOTE: do NOT call zis.closeEntry() on failure paths.
                    // closeEntry() drains (inflates) the rest of the current
                    // entry to seek to the next one — for a zip bomb that
                    // would keep burning CPU/memory on the very entry we're
                    // rejecting. Returning here lets try-with-resources close
                    // the whole stream without draining.
                    return "ERR:unzip:fail entry count exceeds limit " + UNZIP_MAX_ENTRIES;
                }

                File outFile = new File(destDir, entry.getName());

                // Security: path traversal protection (zip slip)
                String canonicalPath = outFile.getCanonicalPath();
                if (!canonicalPath.startsWith(canonicalDest + File.separator)
                        && !canonicalPath.equals(canonicalDest)) {
                    return "ERR:unzip:fail path traversal detected: " + entry.getName();
                }

                // NOTE on symlink divergence vs the Rust path: the Rust
                // extractor rejects symlink entries outright. java.util.zip's
                // ZipInputStream does not surface a zip entry's Unix mode
                // (that lives in the central directory, which ZipInputStream
                // never reads), so a symlink entry here is materialised as a
                // regular file whose contents are the link target string — it
                // is never turned into a real symlink. That is safe (no
                // symlink escape is possible) but behaviourally different
                // from the Rust path. Documented rather than "fixed" because
                // detecting it requires reading the central directory
                // (ZipFile) or a non-stdlib zip library.

                if (entry.isDirectory()) {
                    if (!outFile.exists() && !outFile.mkdirs() && !outFile.isDirectory()) {
                        return "ERR:unzip:fail cannot create directory: " + entry.getName();
                    }
                } else {
                    // Budget: compression-ratio check when both sizes are
                    // known (streamed entries may report -1; the streaming
                    // byte caps below still bound those).
                    long advertised = entry.getSize();
                    long compressed = entry.getCompressedSize();
                    if (UNZIP_MAX_COMPRESSION_RATIO > 0 && advertised > 0 && compressed > 0
                            && advertised / compressed > UNZIP_MAX_COMPRESSION_RATIO) {
                        return "ERR:unzip:fail compression ratio exceeds limit " + UNZIP_MAX_COMPRESSION_RATIO
                                + ": " + entry.getName();
                    }

                    // Ensure parent directories exist. mkdirs() returns false
                    // if the dir already exists, so re-check isDirectory().
                    File parent = outFile.getParentFile();
                    if (parent != null && !parent.isDirectory() && !parent.mkdirs()
                            && !parent.isDirectory()) {
                        return "ERR:unzip:fail cannot create parent directory: " + entry.getName();
                    }

                    long entryWritten = 0;
                    boolean overBudget = false;
                    String budgetErr = null;
                    try (java.io.FileOutputStream fos = new java.io.FileOutputStream(outFile)) {
                        int len;
                        while ((len = zis.read(buffer)) > 0) {
                            entryWritten += len;
                            // Budget: per-entry uncompressed cap.
                            if (entryWritten > UNZIP_MAX_ENTRY_UNCOMPRESSED) {
                                overBudget = true;
                                budgetErr = "ERR:unzip:fail entry exceeds per-entry limit "
                                        + UNZIP_MAX_ENTRY_UNCOMPRESSED + ": " + entry.getName();
                                break;
                            }
                            // Budget: total uncompressed cap across all entries.
                            if (totalWritten + entryWritten > UNZIP_MAX_TOTAL_UNCOMPRESSED) {
                                overBudget = true;
                                budgetErr = "ERR:unzip:fail total uncompressed size exceeds limit "
                                        + UNZIP_MAX_TOTAL_UNCOMPRESSED;
                                break;
                            }
                            fos.write(buffer, 0, len);
                        }
                    }
                    if (overBudget) {
                        // Remove the partial file so a rejected bomb leaves
                        // no half-written entry behind. Do NOT closeEntry()
                        // (see note above): return and let try-with-resources
                        // tear down the stream without draining the rest of
                        // this over-budget entry.
                        //noinspection ResultOfMethodCallIgnored
                        outFile.delete();
                        return budgetErr;
                    }
                    totalWritten += entryWritten;
                    fileCount++;
                }
                zis.closeEntry();
            }
        } catch (java.io.IOException e) {
            return "ERR:unzip:fail " + (e.getMessage() != null ? e.getMessage() : "IO error");
        }

        return String.valueOf(fileCount);
    }

    // ==================== Charset Encoding (GBK) ====================

    /**
     * Encode a string to GBK bytes using Android's built-in java.nio.charset.Charset.
     * Available on all Android API levels (API 1+).
     *
     * @param data The string to encode
     * @return GBK-encoded bytes, or null on error
     */
    public static byte[] encodeGbk(String data) {
        if (data == null) return null;
        try {
            return data.getBytes("GBK");
        } catch (java.io.UnsupportedEncodingException e) {
            return null;
        }
    }

    /**
     * Decode GBK bytes to a string using Android's built-in java.nio.charset.Charset.
     * Available on all Android API levels (API 1+).
     *
     * @param data The GBK-encoded bytes
     * @return Decoded string, or null on error
     */
    public static String decodeGbk(byte[] data) {
        if (data == null) return null;
        try {
            return new String(data, "GBK");
        } catch (java.io.UnsupportedEncodingException e) {
            return null;
        }
    }

    // ==================== System Settings ====================

    /**
     * Open system Bluetooth settings.
     *
     * @param sessionId The session ID to receive the callback
     */
    public static void openSystemBluetoothSetting(int sessionId, int requestId) {
        RuntimeContext context = RuntimeRegistry.get(sessionId);
        if (context == null) {
            NativeMethods.onBluetoothSettingResult(sessionId, requestId, false);
            return;
        }

        Activity activity = context.getActivity();
        if (activity == null) {
            NativeMethods.onBluetoothSettingResult(sessionId, requestId, false);
            return;
        }

        try {
            Intent intent = new Intent(Settings.ACTION_BLUETOOTH_SETTINGS);
            ResultProxyActivity.launch(activity, intent, BLUETOOTH_SETTING_REQUEST_CODE,
                    (requestCode, resultCode, data) -> {
                        android.bluetooth.BluetoothAdapter adapter =
                                android.bluetooth.BluetoothAdapter.getDefaultAdapter();
                        boolean enabled = adapter != null && adapter.isEnabled();
                        NativeMethods.onBluetoothSettingResult(sessionId, requestId, enabled);
                    });
        } catch (Exception e) {
            NativeMethods.onBluetoothSettingResult(sessionId, requestId, false);
        }
    }

    /**
     * Open app authorization (permission) settings page.
     *
     * @param sessionId The session ID to receive the callback
     */
    public static void openAppAuthorizeSetting(int sessionId, int requestId) {
        RuntimeContext context = RuntimeRegistry.get(sessionId);
        if (context == null) {
            NativeMethods.onAppAuthorizeSettingResult(sessionId, requestId, -1);
            return;
        }

        Activity activity = context.getActivity();
        if (activity == null) {
            NativeMethods.onAppAuthorizeSettingResult(sessionId, requestId, -1);
            return;
        }

        try {
            Intent intent = new Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS);
            Uri uri = Uri.fromParts("package", activity.getPackageName(), null);
            intent.setData(uri);
            ResultProxyActivity.launch(activity, intent, APP_AUTHORIZE_SETTING_REQUEST_CODE,
                    (requestCode, resultCode, data) ->
                            NativeMethods.onAppAuthorizeSettingResult(sessionId, requestId, 0));
        } catch (Exception e) {
            NativeMethods.onAppAuthorizeSettingResult(sessionId, requestId, -1);
        }
    }

    // ==================== Window Information ====================

    /**
     * Get window information as a packed byte array.
     * <p>
     * Layout (52 bytes total):
     * - [0-3]:   windowWidth (int)
     * - [4-7]:   windowHeight (int)
     * - [8-11]:  screenWidth (int)
     * - [12-15]: screenHeight (int)
     * - [16-19]: statusBarHeight (int)
     * - [20-23]: pixelRatio * 1000 (int)
     * - [24-27]: screenTop (int)
     * - [28-31]: reserved
     * - [32-35]: reserved
     * - [36-39]: safeAreaLeft (int)
     * - [40-43]: safeAreaTop (int)
     * - [44-47]: safeAreaRight (int)
     * - [48-51]: safeAreaBottom (int)
     *
     * @param sessionId The session ID
     * @return Packed byte array, or null if unavailable
     */
    public static byte[] getWindowInfoBytes(int sessionId) {
        RuntimeContext context = RuntimeRegistry.get(sessionId);
        if (context == null) {
            return null;
        }

        Activity activity = context.getActivity();
        if (activity == null) {
            return null;
        }

        try {
            // Use DisplayCompat for API 21+ compatibility
            int screenWidth = DisplayCompat.getScreenWidth(activity);
            int screenHeight = DisplayCompat.getScreenHeight(activity);
            float density = DisplayCompat.getDensity(activity);
            int statusBarHeight = DisplayCompat.getStatusBarHeight(activity);

            // Window dimensions (use decor view if available)
            int windowWidth = screenWidth;
            int windowHeight = screenHeight;
            int screenTop = 0;

            try {
                android.view.View decorView = activity.getWindow().getDecorView();
                if (decorView.getWidth() > 0) {
                    windowWidth = decorView.getWidth();
                    windowHeight = decorView.getHeight();
                }
                int[] location = new int[2];
                decorView.getLocationOnScreen(location);
                screenTop = location[1];
            } catch (Exception ignored) {
            }

            // Safe area insets using compat layer
            DisplayCompat.SafeAreaInsets safeArea = DisplayCompat.getSafeAreaInsets(activity);

            ByteBuffer buffer = ByteBuffer.allocate(52);
            buffer.order(ByteOrder.LITTLE_ENDIAN);
            buffer.putInt(windowWidth);     // 0-3
            buffer.putInt(windowHeight);    // 4-7
            buffer.putInt(screenWidth);     // 8-11
            buffer.putInt(screenHeight);    // 12-15
            buffer.putInt(statusBarHeight); // 16-19
            buffer.putInt((int) (density * 1000)); // 20-23
            buffer.putInt(screenTop);       // 24-27
            buffer.putInt(0);               // 28-31: reserved
            buffer.putInt(0);               // 32-35: reserved
            buffer.putInt(safeArea.left);   // 36-39
            buffer.putInt(safeArea.top);    // 40-43
            buffer.putInt(safeArea.right);  // 44-47
            buffer.putInt(safeArea.bottom); // 48-51

            return buffer.array();
        } catch (Exception e) {
            return null;
        }
    }

    // ==================== System Settings Info ====================

    /**
     * Get system settings as a packed byte array.
     * <p>
     * Layout (4 bytes):
     * - [0]: bluetoothEnabled (0/1)
     * - [1]: locationEnabled (0/1)
     * - [2]: wifiEnabled (0/1)
     * - [3]: orientation (0=unknown, 1=portrait, 2=landscape)
     *
     * @return Packed byte array
     */
    public static byte[] getSystemSettingInfoBytes(int sessionId) {
        RuntimeContext context = RuntimeRegistry.get(sessionId);
        Activity activity = context != null ? context.getActivity() : null;
        Context appContext = activity != null ? activity : AppContext.getOrNull();

        boolean isLandscape = activity != null && DisplayCompat.isLandscape(activity);
        return SystemSettings.toBytes(appContext, isLandscape);
    }

    // ==================== Device Information ====================

    /**
     * Get device information as JSON string.
     *
     * @return JSON string with device info
     */
    public static String getDeviceInfoJson() {
        Context appContext = AppContext.getOrNull();
        return DeviceInfo.toJson(appContext);
    }

    // ==================== Battery ====================

    /**
     * Get battery info as JSON string.
     *
     * @return JSON string with battery level, charging status, and low power mode
     */
    public static String getBatteryInfoJson() {
        Context appContext = AppContext.getOrNull();
        return BatteryInfo.toJson(appContext);
    }

    // ==================== Vibration ====================

    /**
     * Trigger a short vibration (15ms).
     *
     * @param type Vibration type: "heavy", "medium", or "light"
     * @return 0 on success, -1 if unavailable, -2 if type not supported
     */
    public static int vibrateShort(String type) {
        Context appContext = AppContext.getOrNull();
        return Vibrator.vibrateShort(appContext, type);
    }

    /**
     * Trigger a long vibration (400ms).
     *
     * @return 0 on success, -1 if unavailable
     */
    public static int vibrateLong() {
        Context appContext = AppContext.getOrNull();
        return Vibrator.vibrateLong(appContext);
    }

    // ==================== Screen ====================

    /**
     * Get current screen brightness.
     *
     * @param sessionId The session ID
     * @return Brightness value 0.0-1.0, or -1 if following system
     */
    public static float getScreenBrightness(int sessionId) {
        RuntimeContext context = RuntimeRegistry.get(sessionId);
        if (context == null) return -1f;
        Activity activity = context.getActivity();
        return ScreenBrightness.getBrightness(activity);
    }

    /**
     * Set screen brightness.
     *
     * @param sessionId The session ID
     * @param value     Brightness value (0.0-1.0) or -1 for system default
     * @return 0 on success, -1 on failure
     */
    public static int setScreenBrightness(int sessionId, float value) {
        RuntimeContext context = RuntimeRegistry.get(sessionId);
        if (context == null) return -1;
        Activity activity = context.getActivity();
        return ScreenBrightness.setBrightness(activity, value);
    }

    /**
     * Set whether to keep screen on.
     *
     * @param sessionId    The session ID
     * @param keepScreenOn true to keep screen on
     * @return 0 on success, -1 on failure
     */
    public static int setKeepScreenOn(int sessionId, boolean keepScreenOn) {
        RuntimeContext context = RuntimeRegistry.get(sessionId);
        if (context == null) return -1;
        Activity activity = context.getActivity();
        return ScreenBrightness.setKeepScreenOn(activity, keepScreenOn);
    }

    /**
     * Set device orientation (landscape or portrait).
     *
     * @param sessionId The session ID
     * @param value     "landscape" or "portrait"
     * @return 0 on success, -1 on failure, -2 if value is invalid
     */
    public static int setDeviceOrientation(int sessionId, String value) {
        RuntimeContext context = RuntimeRegistry.get(sessionId);
        if (context == null) return -1;
        Activity activity = context.getActivity();
        return DisplayCompat.setDeviceOrientation(activity, value);
    }

    // ==================== Debug ====================

    /**
     * Set whether debug mode is enabled at runtime.
     *
     * @param sessionId   The session ID
     * @param enableDebug true to enable debug, false to disable
     * @return 0 on success, -1 on failure
     */
    public static int setEnableDebug(int sessionId, boolean enableDebug) {
        GameSession session = sSessions.get(sessionId);
        if (session == null) return -1;
        sMainHandler.post(() -> session.setDebugEnabled(enableDebug));
        return 0;
    }

    // ==================== UI Interaction ====================

    /**
     * Show a toast overlay.
     *
     * @param sessionId The session ID
     * @param json      JSON params: {title, icon, duration, mask}
     */
    public static void showToast(int sessionId, String json) {
        RuntimeContext context = RuntimeRegistry.get(sessionId);
        if (context == null) return;
        Activity activity = context.getActivity();
        if (activity == null) return;
        InteractionUI.showToast(activity, sessionId, json);
    }

    /**
     * Hide the current toast.
     *
     * @param sessionId The session ID
     */
    public static void hideToast(int sessionId) {
        RuntimeContext context = RuntimeRegistry.get(sessionId);
        if (context == null) return;
        Activity activity = context.getActivity();
        if (activity == null) return;
        InteractionUI.hideToast(sessionId);
    }

    /**
     * Show a modal dialog.
     *
     * @param sessionId The session ID
     * @param json      JSON params: {title, content, showCancel, cancelText, confirmText, cancelColor, confirmColor}
     */
    public static void showModal(int sessionId, String json) {
        // Bound before anything can fail, so a refusal still answers the call
        // that asked rather than the oldest one waiting.
        final int requestId = CallbackCorrelation.requestIdOf(json);
        RuntimeContext context = RuntimeRegistry.get(sessionId);
        if (context == null) {
            NativeMethods.onModalResult(sessionId, requestId, 0, 1);
            return;
        }
        Activity activity = context.getActivity();
        if (activity == null) {
            NativeMethods.onModalResult(sessionId, requestId, 0, 1);
            return;
        }
        InteractionUI.showModal(activity, sessionId, json);
    }

    /**
     * Show a loading overlay.
     *
     * @param sessionId The session ID
     * @param json      JSON params: {title, mask}
     */
    public static void showLoading(int sessionId, String json) {
        RuntimeContext context = RuntimeRegistry.get(sessionId);
        if (context == null) return;
        Activity activity = context.getActivity();
        if (activity == null) return;
        InteractionUI.showLoading(activity, sessionId, json);
    }

    /**
     * Hide the current loading overlay.
     *
     * @param sessionId The session ID
     */
    public static void hideLoading(int sessionId) {
        RuntimeContext context = RuntimeRegistry.get(sessionId);
        if (context == null) return;
        Activity activity = context.getActivity();
        if (activity == null) return;
        InteractionUI.hideLoading(sessionId);
    }

    /**
     * Show an action sheet.
     *
     * @param sessionId The session ID
     * @param json      JSON params: {alertText, itemList, itemColor}
     */
    public static void showActionSheet(int sessionId, String json) {
        final int requestId = CallbackCorrelation.requestIdOf(json);
        RuntimeContext context = RuntimeRegistry.get(sessionId);
        if (context == null) {
            NativeMethods.onActionSheetResult(sessionId, requestId, -1);
            return;
        }
        Activity activity = context.getActivity();
        if (activity == null) {
            NativeMethods.onActionSheetResult(sessionId, requestId, -1);
            return;
        }
        InteractionUI.showActionSheet(activity, sessionId, json);
    }

    // ==================== Permissions ====================

    /**
     * Get app authorization settings as JSON string.
     *
     * @return JSON string with permission states
     */
    public static String getAppAuthorizationSettingJson(int sessionId) {
        RuntimeContext runtimeContext = RuntimeRegistry.get(sessionId);
        Context context = runtimeContext != null ? runtimeContext.getActivity() : null;
        if (context == null) {
            context = AppContext.getOrNull();
        }
        return Permissions.toJson(context);
    }

    // ==================== Device Sensor (delegates to SensorExports) ====================

    /**
     * Start listening for device motion (rotation vector) events.
     * Called from native code via JNI.
     *
     * @param sessionId The session ID
     * @param interval  "game", "ui", or "normal"
     */
    public static void startDeviceMotionListening(int sessionId, String interval) {
        SensorExports.startDeviceMotionListening(sessionId, interval);
    }

    /**
     * Stop listening for device motion events.
     * Called from native code via JNI.
     *
     * @param sessionId The session ID
     */
    public static void stopDeviceMotionListening(int sessionId) {
        SensorExports.stopDeviceMotionListening(sessionId);
    }

    /**
     * Start listening for gyroscope events.
     * Called from native code via JNI.
     *
     * @param sessionId The session ID
     * @param interval  "game", "ui", or "normal"
     */
    public static void startGyroscope(int sessionId, String interval) {
        SensorExports.startGyroscope(sessionId, interval);
    }

    /**
     * Stop listening for gyroscope events.
     * Called from native code via JNI.
     *
     * @param sessionId The session ID
     */
    public static void stopGyroscope(int sessionId) {
        SensorExports.stopGyroscope(sessionId);
    }

    /**
     * Start listening for compass events.
     * Called from native code via JNI.
     *
     * @param sessionId The session ID
     */
    public static void startCompass(int sessionId) {
        SensorExports.startCompass(sessionId);
    }

    /**
     * Stop listening for compass events.
     * Called from native code via JNI.
     *
     * @param sessionId The session ID
     */
    public static void stopCompass(int sessionId) {
        SensorExports.stopCompass(sessionId);
    }

    /**
     * Start listening for accelerometer events.
     * Called from native code via JNI.
     *
     * @param sessionId The session ID
     * @param interval  "game", "ui", or "normal"
     */
    public static void startAccelerometer(int sessionId, String interval) {
        SensorExports.startAccelerometer(sessionId, interval);
    }

    /**
     * Stop listening for accelerometer events.
     * Called from native code via JNI.
     *
     * @param sessionId The session ID
     */
    public static void stopAccelerometer(int sessionId) {
        SensorExports.stopAccelerometer(sessionId);
    }

    /**
     * Clean up sensor resources for a session. Call on session shutdown.
     *
     * @param sessionId The session ID
     */
    public static void destroySensorManager(int sessionId) {
        SensorExports.destroySensorManager(sessionId);
    }

    // ==================== Network (delegates to NetworkExports) ====================

    /**
     * Start monitoring network status changes.
     * Called from native code via JNI.
     *
     * @param sessionId The session ID
     */
    public static void startNetworkMonitoring(int sessionId) {
        NetworkExports.startNetworkMonitoring(sessionId);
    }

    /**
     * Stop monitoring network status changes.
     * Called from native code via JNI.
     *
     * @param sessionId The session ID
     */
    public static void stopNetworkMonitoring(int sessionId) {
        NetworkExports.stopNetworkMonitoring(sessionId);
    }

    // ==================== Screen Capture (delegates to SensorExports) ====================

    /**
     * Start observing user screenshot events (lazy, called from JS onUserCaptureScreen).
     *
     * @param sessionId The session ID
     */
    public static void startCaptureScreen(int sessionId) {
        SensorExports.startCaptureScreen(sessionId);
    }

    /**
     * Stop observing user screenshot events (called from JS offUserCaptureScreen).
     *
     * @param sessionId The session ID
     */
    public static void stopCaptureScreen(int sessionId) {
        SensorExports.stopCaptureScreen(sessionId);
    }

    /**
     * Destroy the screen capture observer for a session.
     *
     * @param sessionId The session ID
     * @hide
     */
    public static void destroyCaptureObserver(int sessionId) {
        SensorExports.destroyCaptureObserver(sessionId);
    }

    /**
     * Get current network type as JSON string.
     *
     * @param sessionId The session ID
     * @return JSON string with networkType, isConnected, signalStrength, hasSystemProxy, weakNet
     */
    public static String getNetworkTypeJson(int sessionId) {
        return NetworkExports.getNetworkTypeJson(sessionId);
    }

    /**
     * Get local IP address as JSON string.
     *
     * @return JSON string with localip and netmask
     */
    public static String getLocalIPAddressJson() {
        return NetworkExports.getLocalIPAddressJson();
    }

    /**
     * Clean up network monitor resources for a session. Call on session shutdown.
     *
     * @param sessionId The session ID
     */
    public static void destroyNetworkMonitor(int sessionId) {
        NetworkExports.destroyNetworkMonitor(sessionId);
    }

    // ==================== Audio Platform ====================

    /**
     * Set inner audio options for audio focus and routing.
     *
     * @param sessionId      The session ID
     * @param mixWithOther   If true, duck other audio instead of taking exclusive focus
     * @param obeyMuteSwitch If true, respect the device ringer/mute mode
     * @param speakerOn      If true, route audio output to speaker
     */
    public static void setInnerAudioOption(int sessionId, boolean mixWithOther,
                                           boolean obeyMuteSwitch, boolean speakerOn) {
        RuntimeContext context = RuntimeRegistry.get(sessionId);
        if (context == null) return;
        Activity activity = context.getActivity();
        if (activity == null) return;

        try {
            android.media.AudioManager audioManager =
                    (android.media.AudioManager) activity.getSystemService(Context.AUDIO_SERVICE);
            if (audioManager == null) return;

            // Audio focus: GAIN_TRANSIENT_MAY_DUCK allows mixing, GAIN takes exclusive focus
            int focusGain = mixWithOther
                    ? android.media.AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_MAY_DUCK
                    : android.media.AudioManager.AUDIOFOCUS_GAIN;
            audioManager.requestAudioFocus(null,
                    android.media.AudioManager.STREAM_MUSIC, focusGain);

            // Speaker routing
            audioManager.setSpeakerphoneOn(speakerOn);

            // obeyMuteSwitch: adjust stream type behavior
            // When obeyMuteSwitch is false, use STREAM_MUSIC which ignores ringer mode
            // When true, the app should check ringer mode before playing
            // This is stored and checked at playback time by the audio engine
        } catch (Exception e) {
            // Silently fail - audio options are best-effort
        }
    }

    /**
     * Get available audio input sources.
     * Returns a comma-separated string of supported audio source identifiers
     * matching RecorderManager.start() audioSource param values.
     *
     * @param sessionId The session ID
     * @return Comma-separated audio source identifiers (e.g., "auto,buildInMic,mic,camcorder,voice_recognition,voice_communication")
     */
    public static String getAvailableAudioSources(int sessionId) {
        StringBuilder sb = new StringBuilder();
        // "auto" is always available
        sb.append("auto");

        // Check each MediaRecorder.AudioSource constant
        // DEFAULT (0) maps to "buildInMic"
        sb.append(",buildInMic");

        // MIC (1) - standard microphone
        sb.append(",mic");

        // CAMCORDER (5) - microphone tuned for video recording
        sb.append(",camcorder");

        // VOICE_RECOGNITION (6) - tuned for voice recognition
        sb.append(",voice_recognition");

        // VOICE_COMMUNICATION (7) - tuned for VoIP, includes echo cancellation
        sb.append(",voice_communication");

        return sb.toString();
    }

    // ==================== Clipboard ====================

    /**
     * Set clipboard content.
     * Shows a toast for ~1.5 seconds.
     * Called from native code via JNI.
     *
     * @param sessionId The session ID
     * @param data      The text data to copy
     * @return 0 on success, -1 on failure
     */
    public static int setClipboardData(int sessionId, String data) {
        RuntimeContext ctx = RuntimeRegistry.get(sessionId);
        if (ctx == null) return -1;
        Activity activity = ctx.getActivity();
        if (activity == null) return -1;
        return Clipboard.setClipboardData(activity, data);
    }

    /**
     * Get clipboard content.
     * Called from native code via JNI.
     *
     * @param sessionId The session ID
     * @return The clipboard text content, or empty string if unavailable
     */
    public static String getClipboardData(int sessionId) {
        RuntimeContext ctx = RuntimeRegistry.get(sessionId);
        if (ctx == null) return "";
        Activity activity = ctx.getActivity();
        if (activity == null) return "";
        return Clipboard.getClipboardData(activity);
    }

    // ==================== Recorder (delegates to MediaExports) ====================

    /**
     * Start recording with the given options.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON string with recording options:
     *                    duration, sampleRate, numberOfChannels, encodeBitRate,
     *                    format, frameSize, audioSource
     */
    public static void recorderStart(int sessionId, String optionsJson) {
        MediaExports.recorderStart(sessionId, optionsJson);
    }

    /**
     * Pause recording.
     *
     * @param sessionId The session ID
     */
    public static void recorderPause(int sessionId) {
        MediaExports.recorderPause(sessionId);
    }

    /**
     * Resume recording after pause.
     *
     * @param sessionId The session ID
     */
    public static void recorderResume(int sessionId) {
        MediaExports.recorderResume(sessionId);
    }

    /**
     * Stop recording.
     *
     * @param sessionId The session ID
     */
    public static void recorderStop(int sessionId) {
        MediaExports.recorderStop(sessionId);
    }

    /**
     * Clean up recorder resources for a session. Call on session shutdown.
     *
     * @param sessionId The session ID
     */
    public static void destroyRecorderManager(int sessionId) {
        MediaExports.destroyRecorderManager(sessionId);
    }

    // ==================== Camera (delegates to MediaExports) ====================

    /**
     * Create a camera instance.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON with keys: cameraId, x, y, width, height, devicePosition, flash, size
     * @return JSON result: {"cameraId": <id>} or error JSON
     */
    public static String cameraCreate(int sessionId, String optionsJson) {
        return MediaExports.cameraCreate(sessionId, optionsJson);
    }

    /**
     * Destroy a camera instance.
     *
     * @param sessionId The session ID
     * @param cameraId  The camera instance ID
     */
    public static void cameraDestroy(int sessionId, int cameraId) {
        MediaExports.cameraDestroy(sessionId, cameraId);
    }

    /**
     * Take a photo with the camera.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON with keys: cameraId, quality
     * @return JSON result or error JSON
     */
    public static String cameraTakePhoto(int sessionId, String optionsJson) {
        return MediaExports.cameraTakePhoto(sessionId, optionsJson);
    }

    /**
     * Start video recording.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON with keys: cameraId
     * @return JSON result or error JSON
     */
    public static String cameraStartRecord(int sessionId, String optionsJson) {
        return MediaExports.cameraStartRecord(sessionId, optionsJson);
    }

    /**
     * Stop video recording.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON with keys: cameraId, compressed
     * @return JSON result or error JSON
     */
    public static String cameraStopRecord(int sessionId, String optionsJson) {
        return MediaExports.cameraStopRecord(sessionId, optionsJson);
    }

    /**
     * Set camera zoom level.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON with keys: cameraId, zoom
     * @return JSON result or error JSON
     */
    public static String cameraSetZoom(int sessionId, String optionsJson) {
        return MediaExports.cameraSetZoom(sessionId, optionsJson);
    }

    /**
     * Start listening for camera frame changes.
     *
     * @param sessionId The session ID
     * @param cameraId  The camera instance ID
     */
    public static void cameraListenFrameChange(int sessionId, int cameraId) {
        MediaExports.cameraListenFrameChange(sessionId, cameraId);
    }

    /**
     * Stop listening for camera frame changes.
     *
     * @param sessionId The session ID
     * @param cameraId  The camera instance ID
     */
    public static void cameraCloseFrameChange(int sessionId, int cameraId) {
        MediaExports.cameraCloseFrameChange(sessionId, cameraId);
    }

    /**
     * Clean up all camera resources for a session. Call on session shutdown.
     *
     * @param sessionId The session ID
     */
    public static void destroyCameraManagers(int sessionId) {
        MediaExports.destroyCameraManagers(sessionId);
    }

    /**
     * Extract the "requestId" field from a JSON string as a raw number string.
     * Returns null if not present or not parseable.
     */
    // ==================== Keyboard (delegates to InputExports) ====================

    public static void keyboardShow(int sessionId, String optionsJson) {
        InputExports.keyboardShow(sessionId, optionsJson);
    }

    public static void keyboardHide(int sessionId) {
        InputExports.keyboardHide(sessionId);
    }

    public static void keyboardUpdate(int sessionId, String value) {
        InputExports.keyboardUpdate(sessionId, value);
    }

    /**
     * Clean up Keyboard resources for a session. Call on session shutdown.
     */
    public static void destroyKeyboardManager(int sessionId) {
        InputExports.destroyKeyboardManager(sessionId);
    }

    // ==================== Bluetooth (delegates to BluetoothExports) ====================

    public static void bluetoothOpenAdapter(int sessionId, String optionsJson) {
        BluetoothExports.bluetoothOpenAdapter(sessionId, optionsJson);
    }

    public static void bluetoothCloseAdapter(int sessionId) {
        BluetoothExports.bluetoothCloseAdapter(sessionId);
    }

    public static String bluetoothGetAdapterState(int sessionId) {
        return BluetoothExports.bluetoothGetAdapterState(sessionId);
    }

    public static void bluetoothStartDevicesDiscovery(int sessionId, String optionsJson) {
        BluetoothExports.bluetoothStartDevicesDiscovery(sessionId, optionsJson);
    }

    public static void bluetoothStopDevicesDiscovery(int sessionId) {
        BluetoothExports.bluetoothStopDevicesDiscovery(sessionId);
    }

    public static String bluetoothGetDevices(int sessionId) {
        return BluetoothExports.bluetoothGetDevices(sessionId);
    }

    public static String bluetoothGetConnectedDevices(int sessionId, String optionsJson) {
        return BluetoothExports.bluetoothGetConnectedDevices(sessionId, optionsJson);
    }

    public static void bluetoothMakePair(int sessionId, String optionsJson) {
        BluetoothExports.bluetoothMakePair(sessionId, optionsJson);
    }

    public static void bluetoothIsDevicePaired(int sessionId, String optionsJson) {
        BluetoothExports.bluetoothIsDevicePaired(sessionId, optionsJson);
    }

    public static void bluetoothStartBeaconDiscovery(int sessionId, String optionsJson) {
        BluetoothExports.bluetoothStartBeaconDiscovery(sessionId, optionsJson);
    }

    public static void bluetoothStopBeaconDiscovery(int sessionId) {
        BluetoothExports.bluetoothStopBeaconDiscovery(sessionId);
    }

    public static String bluetoothGetBeacons(int sessionId) {
        return BluetoothExports.bluetoothGetBeacons(sessionId);
    }

    // ---- BLE GATT (delegates to BluetoothExports) ----

    public static void bleCreateConnection(int sessionId, String optionsJson) {
        BluetoothExports.bleCreateConnection(sessionId, optionsJson);
    }

    public static void bleCloseConnection(int sessionId, String optionsJson) {
        BluetoothExports.bleCloseConnection(sessionId, optionsJson);
    }

    public static String bleGetDeviceServices(int sessionId, String optionsJson) {
        return BluetoothExports.bleGetDeviceServices(sessionId, optionsJson);
    }

    public static String bleGetDeviceCharacteristics(int sessionId, String optionsJson) {
        return BluetoothExports.bleGetDeviceCharacteristics(sessionId, optionsJson);
    }

    public static void bleReadCharacteristicValue(int sessionId, String optionsJson) {
        BluetoothExports.bleReadCharacteristicValue(sessionId, optionsJson);
    }

    public static void bleWriteCharacteristicValue(int sessionId, String optionsJson) {
        BluetoothExports.bleWriteCharacteristicValue(sessionId, optionsJson);
    }

    public static void bleNotifyCharacteristicValueChange(int sessionId, String optionsJson) {
        BluetoothExports.bleNotifyCharacteristicValueChange(sessionId, optionsJson);
    }

    public static String bleGetDeviceRSSI(int sessionId, String optionsJson) {
        return BluetoothExports.bleGetDeviceRSSI(sessionId, optionsJson);
    }

    public static void bleSetMTU(int sessionId, String optionsJson) {
        BluetoothExports.bleSetMTU(sessionId, optionsJson);
    }

    public static String bleGetMTU(int sessionId, String optionsJson) {
        return BluetoothExports.bleGetMTU(sessionId, optionsJson);
    }

    /**
     * Clean up Bluetooth resources for a session. Call on session shutdown.
     */
    public static void destroyBluetoothManager(int sessionId) {
        BluetoothExports.destroyBluetoothManager(sessionId);
    }

    // ==================== Image API (delegates to MediaExports) ====================

    public static void imageSaveToPhotosAlbum(int sessionId, String optionsJson) {
        MediaExports.imageSaveToPhotosAlbum(sessionId, optionsJson);
    }

    public static void imagePreviewMedia(int sessionId, String optionsJson) {
        MediaExports.imagePreviewMedia(sessionId, optionsJson);
    }

    public static void imagePreviewImage(int sessionId, String optionsJson) {
        MediaExports.imagePreviewImage(sessionId, optionsJson);
    }

    public static void imageCompress(int sessionId, String optionsJson) {
        MediaExports.imageCompress(sessionId, optionsJson);
    }

    public static void imageChooseMessageFile(int sessionId, String optionsJson) {
        MediaExports.imageChooseMessageFile(sessionId, optionsJson);
    }

    public static void imageChooseImage(int sessionId, String optionsJson) {
        MediaExports.imageChooseImage(sessionId, optionsJson);
    }

    /**
     * Clean up Image API resources for a session. Call on session shutdown.
     */
    public static void destroyImageApiManager(int sessionId) {
        MediaExports.destroyImageApiManager(sessionId);
    }

    // ==================== Location ====================

    /**
     * Start an async location request (getLocation).
     * Result is delivered via {@link NativeMethods#onLocationResult}.
     * Called from native code via JNI.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON with: type, altitude, isHighAccuracy, highAccuracyExpireTime
     */
    public static void getLocation(int sessionId, String optionsJson) {
        RuntimeContext ctx = RuntimeRegistry.get(sessionId);
        if (ctx == null) {
            NativeMethods.onLocationResult(sessionId,
                    CallbackCorrelation.failure(
                            CallbackCorrelation.requestIdOf(optionsJson),
                            "getLocation", "invalid session"));
            return;
        }
        Activity activity = ctx.getActivity();
        if (activity == null) {
            NativeMethods.onLocationResult(sessionId,
                    CallbackCorrelation.failure(
                            CallbackCorrelation.requestIdOf(optionsJson),
                            "getLocation", "no activity"));
            return;
        }
        PermissionOperationGate.Pending pending =
                sPermissionOperations.register(sessionId, "scope.userLocation");
        if (pending == null) {
            NativeMethods.onLocationResult(sessionId,
                    CallbackCorrelation.failure(
                            CallbackCorrelation.requestIdOf(optionsJson),
                            "getLocation", "permission revoked"));
            return;
        }
        if (!sPermissionOperations.enter(pending, () ->
                LocationProvider.getLocationAsync(
                        activity, sessionId, optionsJson, sPermissionOperations, pending,
                        failure -> reportCleanupFailureAndScheduleTerminalClose(
                                sessionId, "location request cleanup", failure)))) {
            NativeMethods.onLocationResult(sessionId,
                    CallbackCorrelation.failure(
                            CallbackCorrelation.requestIdOf(optionsJson),
                            "getLocation", "permission revoked"));
        }
    }

    /**
     * Start an async fuzzy location request (getFuzzyLocation).
     * Result is delivered via {@link NativeMethods#onFuzzyLocationResult}.
     * Called from native code via JNI.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON with: type
     */
    public static void getFuzzyLocation(int sessionId, String optionsJson) {
        RuntimeContext ctx = RuntimeRegistry.get(sessionId);
        if (ctx == null) {
            NativeMethods.onFuzzyLocationResult(sessionId,
                    CallbackCorrelation.failure(
                            CallbackCorrelation.requestIdOf(optionsJson),
                            "getFuzzyLocation", "invalid session"));
            return;
        }
        Activity activity = ctx.getActivity();
        if (activity == null) {
            NativeMethods.onFuzzyLocationResult(sessionId,
                    CallbackCorrelation.failure(
                            CallbackCorrelation.requestIdOf(optionsJson),
                            "getFuzzyLocation", "no activity"));
            return;
        }
        PermissionOperationGate.Pending pending =
                sPermissionOperations.register(sessionId, "scope.userLocation");
        if (pending == null) {
            NativeMethods.onFuzzyLocationResult(sessionId,
                    CallbackCorrelation.failure(
                            CallbackCorrelation.requestIdOf(optionsJson),
                            "getFuzzyLocation", "permission revoked"));
            return;
        }
        if (!sPermissionOperations.enter(pending, () ->
                LocationProvider.getFuzzyLocationAsync(
                        activity, sessionId, optionsJson, sPermissionOperations, pending,
                        failure -> reportCleanupFailureAndScheduleTerminalClose(
                                sessionId, "fuzzy location request cleanup", failure)))) {
            NativeMethods.onFuzzyLocationResult(sessionId,
                    CallbackCorrelation.failure(
                            CallbackCorrelation.requestIdOf(optionsJson),
                            "getFuzzyLocation", "permission revoked"));
        }
    }

    // ==================== Scan Code (delegates to InputExports) ====================

    public static void scanCode(int sessionId, String optionsJson) {
        InputExports.scanCode(sessionId, optionsJson);
    }

    public static void destroyScanCodeManager(int sessionId) {
        InputExports.destroyScanCodeManager(sessionId);
    }

    // ==================== Video (delegates to MediaExports) ====================

    public static String videoCreate(int sessionId, String optionsJson) {
        return MediaExports.videoCreate(sessionId, optionsJson);
    }

    public static void videoPlay(int sessionId, int videoId) {
        MediaExports.videoPlay(sessionId, videoId);
    }

    public static void videoPause(int sessionId, int videoId) {
        MediaExports.videoPause(sessionId, videoId);
    }

    public static void videoStop(int sessionId, int videoId) {
        MediaExports.videoStop(sessionId, videoId);
    }

    public static void videoSeek(int sessionId, String json) {
        MediaExports.videoSeek(sessionId, json);
    }

    public static void videoRequestFullscreen(int sessionId, String json) {
        MediaExports.videoRequestFullscreen(sessionId, json);
    }

    public static void videoExitFullscreen(int sessionId, int videoId) {
        MediaExports.videoExitFullscreen(sessionId, videoId);
    }

    public static void videoSetProperty(int sessionId, String json) {
        MediaExports.videoSetProperty(sessionId, json);
    }

    public static void videoDestroy(int sessionId, int videoId) {
        MediaExports.videoDestroy(sessionId, videoId);
    }

    /**
     * Clean up Video resources for a session. Call on session shutdown.
     */
    public static void destroyVideoManager(int sessionId) {
        MediaExports.destroyVideoManager(sessionId);
    }

    // ==================== Game Log ====================

    private static final String GAME_LOG_TAG = "MigoGameLog";

    /**
     * Set or clear the game log handler for a session.
     *
     * @hide Called by {@link com.migo.runtime.GameSession#setGameLogHandler(GameLogHandler)}.
     */
    public static void setGameLogHandler(int sessionId, GameLogHandler handler) {
        if (handler == null) {
            sGameLogHandlers.remove(sessionId);
        } else {
            sGameLogHandlers.put(sessionId, handler);
        }
    }

    public static void gameLogReport(int sessionId, String logJson) {
        GameLogHandler h = sGameLogHandlers.get(sessionId);
        if (h != null) {
            h.onLog(logJson);
        } else {
            android.util.Log.d(GAME_LOG_TAG, "[session=" + sessionId + "] " + logJson);
        }
    }

    private static void clearGameLogHandler(int sessionId) {
        sGameLogHandlers.remove(sessionId);
    }

    // ==================== Auth ====================

    /**
     * Set or clear auth handler for a session.
     *
     * @hide Called by {@link com.migo.runtime.GameSession#setAuthHandler(AuthHandler)}.
     */
    public static void setAuthHandler(int sessionId, AuthHandler handler) {
        if (handler == null) {
            sAuthHandlers.remove(sessionId);
        } else {
            sAuthHandlers.put(sessionId, handler);
        }
    }

    private static void clearAuthHandler(int sessionId) {
        sAuthHandlers.remove(sessionId);
    }

    private static JSONObject parseAuthOptions(String optionsJson) {
        if (optionsJson == null || optionsJson.isEmpty()) {
            return new JSONObject();
        }
        try {
            return new JSONObject(optionsJson);
        } catch (Exception ignored) {
            return new JSONObject();
        }
    }

    private static int parseAuthRequestId(JSONObject options) {
        return options != null ? options.optInt("requestId", 0) : 0;
    }

    private static int parseAuthTimeout(JSONObject options) {
        return options != null ? options.optInt("timeout", 0) : 0;
    }

    private static boolean parseAuthBoolean(JSONObject options, String key, boolean defaultValue) {
        return options != null ? options.optBoolean(key, defaultValue) : defaultValue;
    }

    private static String parseAuthLang(JSONObject options) {
        String lang = options != null ? options.optString("lang", "en") : "en";
        if ("zh_CN".equals(lang) || "zh_TW".equals(lang) || "en".equals(lang)) {
            return lang;
        }
        return "en";
    }

    private static String normalizeAuthError(String reason) {
        if (reason == null) {
            return "unknown error";
        }
        String trimmed = reason.trim();
        return trimmed.isEmpty() ? "unknown error" : trimmed;
    }

    private static void reportLoginSuccess(int sessionId, int requestId, String code) {
        try {
            JSONObject res = new JSONObject();
            res.put("requestId", requestId);
            res.put("code", code);
            NativeMethods.onLoginResult(sessionId, res.toString());
        } catch (JSONException ignored) {
            NativeMethods.onLoginResult(sessionId,
                    "{\"requestId\":" + requestId + ",\"error\":\"internal error\"}");
        }
    }

    private static void reportLoginFail(int sessionId, int requestId, String reason) {
        try {
            JSONObject res = new JSONObject();
            res.put("requestId", requestId);
            res.put("error", normalizeAuthError(reason));
            NativeMethods.onLoginResult(sessionId, res.toString());
        } catch (JSONException ignored) {
            NativeMethods.onLoginResult(sessionId,
                    "{\"requestId\":" + requestId + ",\"error\":\"internal error\"}");
        }
    }

    private static void reportCheckSessionSuccess(int sessionId, int requestId) {
        NativeMethods.onCheckSessionResult(sessionId,
                "{\"requestId\":" + requestId + "}");
    }

    private static void reportCheckSessionFail(int sessionId, int requestId, String reason) {
        try {
            JSONObject res = new JSONObject();
            res.put("requestId", requestId);
            res.put("error", normalizeAuthError(reason));
            NativeMethods.onCheckSessionResult(sessionId, res.toString());
        } catch (JSONException ignored) {
            NativeMethods.onCheckSessionResult(sessionId,
                    "{\"requestId\":" + requestId + ",\"error\":\"internal error\"}");
        }
    }

    private static void reportGetUserInfoSuccess(
            int sessionId,
            int requestId,
            AuthHandler.UserInfoResult payload,
            boolean withCredentials,
            String lang
    ) {
        try {
            JSONObject res = new JSONObject();
            res.put("requestId", requestId);

            AuthHandler.UserInfoResult source = payload != null ? payload : new AuthHandler.UserInfoResult();
            AuthHandler.UserInfo userInfo = source.userInfo != null ? source.userInfo : new AuthHandler.UserInfo();

            JSONObject user = new JSONObject();
            user.put("nickName", userInfo.nickName != null ? userInfo.nickName : "");
            user.put("avatarUrl", userInfo.avatarUrl != null ? userInfo.avatarUrl : "");
            user.put("gender", userInfo.gender);
            user.put("country", userInfo.country != null ? userInfo.country : "");
            user.put("province", userInfo.province != null ? userInfo.province : "");
            user.put("city", userInfo.city != null ? userInfo.city : "");
            user.put("language", userInfo.language != null ? userInfo.language : lang);
            res.put("userInfo", user);

            if (source.rawData != null && !source.rawData.isEmpty()) {
                res.put("rawData", source.rawData);
            }
            if (source.signature != null && !source.signature.isEmpty()) {
                res.put("signature", source.signature);
            }
            if (withCredentials) {
                if (source.encryptedData != null && !source.encryptedData.isEmpty()) {
                    res.put("encryptedData", source.encryptedData);
                }
                if (source.iv != null && !source.iv.isEmpty()) {
                    res.put("iv", source.iv);
                }
            }
            if (source.cloudID != null && !source.cloudID.isEmpty()) {
                res.put("cloudID", source.cloudID);
            }

            NativeMethods.onGetUserInfoResult(sessionId, res.toString());
        } catch (JSONException ignored) {
            NativeMethods.onGetUserInfoResult(sessionId,
                    "{\"requestId\":" + requestId + ",\"error\":\"internal error\"}");
        }
    }

    private static void reportGetUserInfoFail(int sessionId, int requestId, String reason) {
        try {
            JSONObject res = new JSONObject();
            res.put("requestId", requestId);
            res.put("error", normalizeAuthError(reason));
            NativeMethods.onGetUserInfoResult(sessionId, res.toString());
        } catch (JSONException ignored) {
            NativeMethods.onGetUserInfoResult(sessionId,
                    "{\"requestId\":" + requestId + ",\"error\":\"internal error\"}");
        }
    }

    private static void reportGetPhoneNumberSuccess(int sessionId, int requestId, String code) {
        try {
            JSONObject res = new JSONObject();
            res.put("requestId", requestId);
            res.put("code", code);
            NativeMethods.onGetPhoneNumberResult(sessionId, res.toString());
        } catch (JSONException ignored) {
            NativeMethods.onGetPhoneNumberResult(sessionId,
                    "{\"requestId\":" + requestId + ",\"error\":\"internal error\"}");
        }
    }

    private static void reportGetPhoneNumberFail(int sessionId, int requestId, String reason, Integer errno) {
        try {
            JSONObject res = new JSONObject();
            res.put("requestId", requestId);
            res.put("error", normalizeAuthError(reason));
            if (errno != null) {
                res.put("errno", errno.intValue());
            }
            NativeMethods.onGetPhoneNumberResult(sessionId, res.toString());
        } catch (JSONException ignored) {
            NativeMethods.onGetPhoneNumberResult(sessionId,
                    "{\"requestId\":" + requestId + ",\"error\":\"internal error\"}");
        }
    }

    /**
     * Trigger host-side login.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON: {"requestId":N,"timeout":ms}
     */
    public static void authLogin(int sessionId, String optionsJson) {
        JSONObject options = parseAuthOptions(optionsJson);
        int requestId = parseAuthRequestId(options);
        int timeout = parseAuthTimeout(options);

        RuntimeContext ctx = RuntimeRegistry.get(sessionId);
        if (ctx == null) {
            clearAuthHandler(sessionId);
            reportLoginFail(sessionId, requestId, "invalid session");
            return;
        }

        AuthHandler handler = sAuthHandlers.get(sessionId);
        if (handler == null) {
            reportLoginFail(sessionId, requestId, "no auth handler");
            return;
        }

        final java.util.concurrent.atomic.AtomicBoolean done = new java.util.concurrent.atomic.AtomicBoolean(false);

        try {
            handler.login(timeout, new AuthHandler.LoginCallback() {
                @Override
                public void onSuccess(String code) {
                    if (!done.compareAndSet(false, true)) {
                        return;
                    }
                    if (code == null || code.isEmpty()) {
                        reportLoginFail(sessionId, requestId, "invalid code");
                        return;
                    }
                    reportLoginSuccess(sessionId, requestId, code);
                }

                @Override
                public void onFailure(String reason) {
                    if (!done.compareAndSet(false, true)) {
                        return;
                    }
                    reportLoginFail(sessionId, requestId, reason);
                }
            });
        } catch (Exception e) {
            if (done.compareAndSet(false, true)) {
                reportLoginFail(sessionId, requestId,
                        e.getMessage() != null ? e.getMessage() : "unknown error");
            }
        }
    }

    /**
     * Trigger host-side checkSession.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON: {"requestId":N}
     */
    public static void authCheckSession(int sessionId, String optionsJson) {
        JSONObject options = parseAuthOptions(optionsJson);
        int requestId = parseAuthRequestId(options);

        RuntimeContext ctx = RuntimeRegistry.get(sessionId);
        if (ctx == null) {
            clearAuthHandler(sessionId);
            reportCheckSessionFail(sessionId, requestId, "invalid session");
            return;
        }

        AuthHandler handler = sAuthHandlers.get(sessionId);
        if (handler == null) {
            reportCheckSessionFail(sessionId, requestId, "no auth handler");
            return;
        }

        final java.util.concurrent.atomic.AtomicBoolean done = new java.util.concurrent.atomic.AtomicBoolean(false);

        try {
            handler.checkSession(new AuthHandler.CheckSessionCallback() {
                @Override
                public void onSuccess() {
                    if (!done.compareAndSet(false, true)) {
                        return;
                    }
                    reportCheckSessionSuccess(sessionId, requestId);
                }

                @Override
                public void onFailure(String reason) {
                    if (!done.compareAndSet(false, true)) {
                        return;
                    }
                    reportCheckSessionFail(sessionId, requestId, reason);
                }
            });
        } catch (Exception e) {
            if (done.compareAndSet(false, true)) {
                reportCheckSessionFail(sessionId, requestId,
                        e.getMessage() != null ? e.getMessage() : "unknown error");
            }
        }
    }

    /**
     * Trigger host-side getUserInfo.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON: {"requestId":N,"withCredentials":bool,"lang":"en|zh_CN|zh_TW"}
     */
    public static void authGetUserInfo(int sessionId, String optionsJson) {
        JSONObject options = parseAuthOptions(optionsJson);
        int requestId = parseAuthRequestId(options);
        boolean withCredentials = parseAuthBoolean(options, "withCredentials", false);
        String lang = parseAuthLang(options);

        RuntimeContext ctx = RuntimeRegistry.get(sessionId);
        if (ctx == null) {
            clearAuthHandler(sessionId);
            reportGetUserInfoFail(sessionId, requestId, "invalid session");
            return;
        }

        AuthHandler handler = sAuthHandlers.get(sessionId);
        if (handler == null) {
            reportGetUserInfoFail(sessionId, requestId, "no auth handler");
            return;
        }

        final java.util.concurrent.atomic.AtomicBoolean done = new java.util.concurrent.atomic.AtomicBoolean(false);

        try {
            handler.getUserInfo(withCredentials, lang, new AuthHandler.UserInfoCallback() {
                @Override
                public void onSuccess(AuthHandler.UserInfoResult result) {
                    if (!done.compareAndSet(false, true)) {
                        return;
                    }
                    reportGetUserInfoSuccess(sessionId, requestId, result, withCredentials, lang);
                }

                @Override
                public void onFailure(String reason) {
                    if (!done.compareAndSet(false, true)) {
                        return;
                    }
                    reportGetUserInfoFail(sessionId, requestId, reason);
                }
            });
        } catch (Exception e) {
            if (done.compareAndSet(false, true)) {
                reportGetUserInfoFail(sessionId, requestId,
                        e.getMessage() != null ? e.getMessage() : "unknown error");
            }
        }
    }

    /**
     * Trigger host-side getPhoneNumber.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON: {"requestId":N,"isRealtime":bool,"phoneNumberNoQuotaToast":bool}
     */
    public static void authGetPhoneNumber(int sessionId, String optionsJson) {
        JSONObject options = parseAuthOptions(optionsJson);
        int requestId = parseAuthRequestId(options);
        boolean isRealtime = parseAuthBoolean(options, "isRealtime", false);
        boolean phoneNumberNoQuotaToast = parseAuthBoolean(options, "phoneNumberNoQuotaToast", true);

        RuntimeContext ctx = RuntimeRegistry.get(sessionId);
        if (ctx == null) {
            clearAuthHandler(sessionId);
            reportGetPhoneNumberFail(sessionId, requestId, "invalid session", null);
            return;
        }

        AuthHandler handler = sAuthHandlers.get(sessionId);
        if (handler == null) {
            reportGetPhoneNumberFail(sessionId, requestId, "no auth handler", null);
            return;
        }

        final java.util.concurrent.atomic.AtomicBoolean done = new java.util.concurrent.atomic.AtomicBoolean(false);

        try {
            handler.getPhoneNumber(isRealtime, phoneNumberNoQuotaToast, new AuthHandler.PhoneNumberCallback() {
                @Override
                public void onSuccess(String code) {
                    if (!done.compareAndSet(false, true)) {
                        return;
                    }
                    if (code == null || code.isEmpty()) {
                        reportGetPhoneNumberFail(sessionId, requestId, "invalid code", null);
                        return;
                    }
                    reportGetPhoneNumberSuccess(sessionId, requestId, code);
                }

                @Override
                public void onFailure(String reason, Integer errno) {
                    if (!done.compareAndSet(false, true)) {
                        return;
                    }
                    reportGetPhoneNumberFail(sessionId, requestId, reason, errno);
                }
            });
        } catch (Exception e) {
            if (done.compareAndSet(false, true)) {
                reportGetPhoneNumberFail(sessionId, requestId,
                        e.getMessage() != null ? e.getMessage() : "unknown error", null);
            }
        }
    }

    // ==================== Subpackage ====================

    /**
     * Set or clear the subpackage download handler for a session.
     *
     * @hide Called by {@link com.migo.runtime.GameSession#setSubpackageHandler(SubpackageHandler)}.
     */
    public static void setSubpackageHandler(int sessionId, SubpackageHandler handler) {
        if (handler == null) {
            sSubpackageHandlers.remove(sessionId);
        } else {
            sSubpackageHandlers.put(sessionId, handler);
        }
    }

    private static void clearSubpackageHandler(int sessionId) {
        sSubpackageHandlers.remove(sessionId);
    }

    /**
     * Trigger a subpackage download.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON: {"requestId":N,"name":"stage1","root":"subpackages/stage1"}
     */
    public static void subpackageDownload(int sessionId, String optionsJson) {
        JSONObject options = parseAuthOptions(optionsJson);
        int requestId = parseAuthRequestId(options);
        String name = options.optString("name", "");
        String root = options.optString("root", "");

        SubpackageHandler handler = sSubpackageHandlers.get(sessionId);
        if (handler == null) {
            NativeMethods.onSubpackageResult(sessionId,
                    "{\"requestId\":" + requestId + ",\"error\":\"no subpackage handler\"}");
            return;
        }

        final java.util.concurrent.atomic.AtomicBoolean done = new java.util.concurrent.atomic.AtomicBoolean(false);

        try {
            handler.download(
                    new SubpackageHandler.SubpackageRequest(name, root),
                    new SubpackageHandler.DownloadCallback() {
                        @Override
                        public void onProgress(int progress, long totalBytesWritten, long totalBytesExpectedToWrite) {
                            try {
                                JSONObject res = new JSONObject();
                                res.put("requestId", requestId);
                                res.put("progress", progress);
                                res.put("totalBytesWritten", totalBytesWritten);
                                res.put("totalBytesExpectedToWrite", totalBytesExpectedToWrite);
                                NativeMethods.onSubpackageProgress(sessionId, res.toString());
                            } catch (JSONException ignored) {
                            }
                        }

                        @Override
                        public void onSuccess(String zipPath) {
                            if (!done.compareAndSet(false, true)) return;
                            try {
                                JSONObject res = new JSONObject();
                                res.put("requestId", requestId);
                                res.put("zipPath", zipPath != null ? zipPath : "");
                                NativeMethods.onSubpackageResult(sessionId, res.toString());
                            } catch (JSONException ignored) {
                                NativeMethods.onSubpackageResult(sessionId,
                                        "{\"requestId\":" + requestId + ",\"error\":\"json error\"}");
                            }
                        }

                        @Override
                        public void onFailure(String reason) {
                            if (!done.compareAndSet(false, true)) return;
                            try {
                                JSONObject res = new JSONObject();
                                res.put("requestId", requestId);
                                res.put("error", reason != null ? reason : "download failed");
                                NativeMethods.onSubpackageResult(sessionId, res.toString());
                            } catch (JSONException ignored) {
                                NativeMethods.onSubpackageResult(sessionId,
                                        "{\"requestId\":" + requestId + ",\"error\":\"download failed\"}");
                            }
                        }
                    });
        } catch (Exception e) {
            if (done.compareAndSet(false, true)) {
                try {
                    JSONObject res = new JSONObject();
                    res.put("requestId", requestId);
                    res.put("error", e.getMessage() != null ? e.getMessage() : "unknown error");
                    NativeMethods.onSubpackageResult(sessionId, res.toString());
                } catch (JSONException ignored) {
                    NativeMethods.onSubpackageResult(sessionId,
                            "{\"requestId\":" + requestId + ",\"error\":\"unknown error\"}");
                }
            }
        }
    }

    // ==================== Commercial host callbacks ====================
    //
    // Settings, share, navigation and payment. The runtime owns none of them:
    // it has no settings screen, no social graph, no app registry and no
    // merchant credentials, so everything below is transport -- a parsed
    // request out to the embedder's handler, one settlement back. Registering
    // no handler is the ordinary state of an integration in progress, and it
    // settles as "not supported" rather than staying silent, because a pending
    // request content is awaiting is a stalled game.

    /**
     * Set or clear the setting handler for a session.
     *
     * @hide Called by {@link com.migo.runtime.GameSession#setSettingHandler(SettingHandler)}.
     */
    public static void setSettingHandler(int sessionId, SettingHandler handler) {
        if (handler == null) {
            sSettingHandlers.remove(sessionId);
        } else {
            sSettingHandlers.put(sessionId, handler);
        }
    }

    /**
     * Set or clear the share handler for a session.
     *
     * @hide Called by {@link com.migo.runtime.GameSession#setShareHandler(ShareHandler)}.
     */
    public static void setShareHandler(int sessionId, ShareHandler handler) {
        if (handler == null) {
            sShareHandlers.remove(sessionId);
        } else {
            sShareHandlers.put(sessionId, handler);
        }
    }

    /**
     * Set or clear the navigation handler for a session.
     *
     * @hide Called by
     * {@link com.migo.runtime.GameSession#setNavigationHandler(NavigationHandler)}.
     */
    public static void setNavigationHandler(int sessionId, NavigationHandler handler) {
        if (handler == null) {
            sNavigationHandlers.remove(sessionId);
        } else {
            sNavigationHandlers.put(sessionId, handler);
        }
    }

    /**
     * Set or clear the payment handler for a session.
     *
     * @hide Called by {@link com.migo.runtime.GameSession#setPaymentHandler(PaymentHandler)}.
     */
    public static void setPaymentHandler(int sessionId, PaymentHandler handler) {
        if (handler == null) {
            sPaymentHandlers.remove(sessionId);
        } else {
            sPaymentHandlers.put(sessionId, handler);
        }
    }

    private static void clearSettingHandler(int sessionId) {
        sSettingHandlers.remove(sessionId);
    }

    private static void clearShareHandler(int sessionId) {
        sShareHandlers.remove(sessionId);
    }

    private static void clearNavigationHandler(int sessionId) {
        sNavigationHandlers.remove(sessionId);
    }

    private static void clearPaymentHandler(int sessionId) {
        sPaymentHandlers.remove(sessionId);
    }

    /**
     * Claim one deferred request, so that whatever answers it answers once.
     *
     * <p>Answering is unconditional from here on: the handler settles it, the absence of a
     * handler settles it, and a handler that throws settles it too. There is no path that
     * returns without content being told something.
     */
    private static HostDelegation.Settlement settlement(
            int sessionId, JSONObject options, HostDelegation.ResultChannel channel) {
        return new HostDelegation.Settlement(
                sessionId,
                CallbackCorrelation.requestIdOf(options),
                channel,
                () -> isSessionTerminated(sessionId));
    }

    /**
     * Hand a request to a handler, settling it if there is none or if it throws.
     *
     * <p>A host exception is a host bug, but content cannot be left holding a pending
     * request because of it -- the same trade the ad bridge makes.
     */
    private static <H> void delegate(
            int sessionId,
            ConcurrentHashMap<Integer, H> handlers,
            String api,
            HostDelegation.Settlement settlement,
            java.util.function.Consumer<H> invoke) {
        H handler = handlers.get(sessionId);
        if (handler == null) {
            settlement.fail(-2, api + ":fail not supported");
            return;
        }
        try {
            invoke.accept(handler);
        } catch (Exception thrown) {
            android.util.Log.w(TAG, api + ": handler threw: " + thrown);
            settlement.fail(-1, api + ":fail handler threw: " + thrown);
        }
    }

    // ==================== Setting ====================

    /**
     * Open the mini program setting page.
     * Delegates to the session's {@link SettingHandler}.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON options
     */
    public static void openSetting(int sessionId, String optionsJson) {
        JSONObject options = HostDelegation.options(optionsJson);
        HostDelegation.Settlement settlement =
                settlement(sessionId, options, NativeMethods::onOpenSettingResult);
        delegate(sessionId, sSettingHandlers, "openSetting", settlement,
                handler -> handler.openSetting(HostDelegation.settingSink(settlement)));
    }

    // ==================== Share ====================

    /**
     * Trigger the native share flow.
     * Delegates to the session's {@link ShareHandler}.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON with title, imageUrl, query
     */
    public static void shareAppMessage(int sessionId, String optionsJson) {
        JSONObject options = HostDelegation.options(optionsJson);
        HostDelegation.Settlement settlement =
                settlement(sessionId, options, NativeMethods::onShareAppMessageResult);
        delegate(sessionId, sShareHandlers, "shareAppMessage", settlement,
                handler -> handler.shareAppMessage(
                        HostDelegation.shareRequest(options),
                        HostDelegation.shareSink(settlement)));
    }

    // ==================== Navigate ====================

    /**
     * Navigate to another mini program.
     * Delegates to the session's {@link NavigationHandler}.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON with appId, path, extraData, envVersion
     */
    public static void navigateToMiniProgram(int sessionId, String optionsJson) {
        JSONObject options = HostDelegation.options(optionsJson);
        HostDelegation.Settlement settlement =
                settlement(sessionId, options, NativeMethods::onNavigateToMiniProgramResult);
        delegate(sessionId, sNavigationHandlers, "navigateToMiniProgram", settlement,
                handler -> handler.navigateToMiniProgram(
                        HostDelegation.navigateRequest(options),
                        HostDelegation.navigationSink(settlement)));
    }

    /**
     * Open the customer service conversation.
     * Delegates to the session's {@link NavigationHandler}.
     *
     * <p>Alone among its siblings this has no result channel to settle on: the engine
     * defines none for it, and content's call resolves off this method's return across JNI
     * rather than off a later callback. So a refusal is reported by throwing, which is what
     * the engine's void call converts into the {@code fail} content sees -- and why a host
     * that cannot open the conversation must return {@code false} rather than throw its own
     * exception, since only this frame knows the message content expects.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON with sessionFrom, showMessageCard, etc.
     */
    public static void openCustomerServiceConversation(int sessionId, String optionsJson) {
        NavigationHandler handler = sNavigationHandlers.get(sessionId);
        boolean opened = false;
        if (handler != null && !isSessionTerminated(sessionId)) {
            try {
                opened = handler.openCustomerServiceConversation(
                        HostDelegation.customerServiceRequest(
                                HostDelegation.options(optionsJson)));
            } catch (Exception thrown) {
                android.util.Log.w(TAG,
                        "openCustomerServiceConversation: handler threw: " + thrown);
            }
        }
        if (!opened) {
            throw new UnsupportedOperationException(
                    "openCustomerServiceConversation:fail not supported");
        }
    }

    // ==================== Payment ====================

    /**
     * Check if the current environment supports Midas payment.
     * Delegates to the session's {@link PaymentHandler}.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON options
     * @return JSON string: {"data":{"allow_pay":true/false}}
     */
    public static String checkIsSupportMidasPayment(int sessionId, String optionsJson) {
        PaymentHandler handler = sPaymentHandlers.get(sessionId);
        boolean allowed = false;
        if (handler != null && !isSessionTerminated(sessionId)) {
            try {
                allowed = handler.isMidasPaymentSupported();
            } catch (Exception thrown) {
                android.util.Log.w(TAG, "checkIsSupportMidasPayment: handler threw: " + thrown);
            }
        }
        return "{\"data\":{\"allow_pay\":" + allowed + "}}";
    }

    /**
     * Trigger Midas payment flow.
     * Delegates to the session's {@link PaymentHandler}.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON with mode, env, offerId, currencyType, etc.
     */
    public static void requestMidasPayment(int sessionId, String optionsJson) {
        JSONObject options = HostDelegation.options(optionsJson);
        HostDelegation.Settlement settlement =
                settlement(sessionId, options, NativeMethods::onMidasPaymentResult);
        delegate(sessionId, sPaymentHandlers, "requestMidasPayment", settlement,
                handler -> handler.requestMidasPayment(
                        HostDelegation.paymentRequest(options),
                        HostDelegation.paymentSink(settlement)));
    }

    /**
     * Trigger Midas payment for game items.
     * Delegates to the session's {@link PaymentHandler}.
     *
     * @param sessionId   The session ID
     * @param optionsJson JSON with signData, paySig, signature
     */
    public static void requestMidasPaymentGameItem(int sessionId, String optionsJson) {
        JSONObject options = HostDelegation.options(optionsJson);
        HostDelegation.Settlement settlement =
                settlement(sessionId, options, NativeMethods::onMidasPaymentGameItemResult);
        delegate(sessionId, sPaymentHandlers, "requestMidasPaymentGameItem", settlement,
                handler -> handler.requestMidasPaymentGameItem(
                        HostDelegation.gameItemPaymentRequest(options),
                        HostDelegation.paymentSink(settlement)));
    }

    // ---- ADPF Thermal Management ----
    private static final ConcurrentHashMap<Integer, AdpfManager> sAdpfManagers =
            new ConcurrentHashMap<>();

    /**
     * Get or create the ADPF manager for a session, starting thermal monitoring.
     * Safe to call on API < 29: the manager will be a no-op.
     *
     * @param sessionId The session ID
     * @return The AdpfManager, or null if the session context is not available
     */
    public static AdpfManager getOrCreateAdpfManager(int sessionId) {
        AdpfManager existing = sAdpfManagers.get(sessionId);
        if (existing != null) return existing;
        RuntimeContext ctx = RuntimeRegistry.get(sessionId);
        if (ctx == null) return null;
        Activity activity = ctx.getActivity();
        if (activity == null) return null;
        AdpfManager mgr = new AdpfManager(sessionId, activity);
        sAdpfManagers.put(sessionId, mgr);
        mgr.start();
        return mgr;
    }

    /**
     * Destroy the ADPF manager for a session, removing the thermal listener.
     *
     * @param sessionId The session ID
     */
    public static void destroyAdpfManager(int sessionId) {
        ResourceCleanup.destroyMatching(
                sAdpfManagers,
                id -> id == sessionId,
                AdpfManager::destroy);
    }

    /**
     * Get current thermal status for a session (0-6). Returns 0 if unavailable.
     *
     * @param sessionId The session ID
     * @return Thermal status level
     */
    public static int getThermalStatus(int sessionId) {
        AdpfManager mgr = sAdpfManagers.get(sessionId);
        return mgr != null ? mgr.getThermalStatus() : 0;
    }

    /**
     * Destroy all per-session managers. Called from GameSession.close().
     * This is the single cleanup entry point to prevent resource leaks.
     *
     * @param sessionId The session ID
     */
    public static void destroyAllManagers(int sessionId) {
        ResourceCleanup.runAll(
                () -> {
                    if (BuildConfig.MIGO_API_SENSORS) SensorExports.destroyAll(sessionId);
                },
                () -> {
                    if (BuildConfig.MIGO_API_SENSORS) NetworkExports.destroyAll(sessionId);
                },
                () -> {
                    if (BuildConfig.MIGO_API_MEDIA) MediaExports.destroyAll(sessionId);
                },
                () -> InputExports.destroyAll(sessionId),
                // Overlays are views this session put in its Activity's decor.
                // Left here, a session closing with a toast up holds a destroyed
                // Activity alive through a static reference.
                () -> InteractionUI.destroy(sessionId),
                () -> {
                    if (BuildConfig.MIGO_API_CONNECTIVITY) BluetoothExports.destroyAll(sessionId);
                },
                () -> clearGameLogHandler(sessionId),
                () -> clearAuthHandler(sessionId),
                () -> clearSubpackageHandler(sessionId),
                () -> clearMessageHandler(sessionId),
                () -> clearAdHandler(sessionId),
                () -> sAdSinks.remove(sessionId),
                () -> clearSettingHandler(sessionId),
                () -> clearShareHandler(sessionId),
                () -> clearNavigationHandler(sessionId),
                () -> clearPaymentHandler(sessionId),
                () -> clearPermissionHandler(sessionId),
                () -> sPermissionSinks.remove(sessionId),
                () -> unregisterErrorCallback(sessionId),
                () -> destroyAdpfManager(sessionId));
    }

    // ==================== Host <-> JS Message Channel ====================

    /**
     * Register a message handler for a session.
     * Called from {@link GameSession#setMessageHandler}.
     *
     * @param sessionId The session ID
     * @param handler   The handler to receive messages, or null to remove
     * @hide
     */
    public static void registerMessageHandler(int sessionId, GameSession.MessageHandler handler) {
        if (handler != null) {
            sMessageHandlers.put(sessionId, handler);
        } else {
            sMessageHandlers.remove(sessionId);
        }
    }

    /**
     * Remove the message handler for a session.
     *
     * @param sessionId The session ID
     * @hide
     */
    public static void clearMessageHandler(int sessionId) {
        sMessageHandlers.remove(sessionId);
    }

    /**
     * Called from native code (Rust) when JS calls {@code migo.sendToHost(type, payload)}.
     * <p>
     * Parses the JSON envelope and dispatches to the registered {@link GameSession.MessageHandler}
     * on the main thread.
     * <p>
     * JNI signature: {@code (ILjava/lang/String;)V}
     *
     * @param hostId The session/host ID
     * @param json   JSON string with fields "type" and "payload"
     */
    public static void onHostMessage(int hostId, String json) {
        GameSession.MessageHandler handler = sMessageHandlers.get(hostId);
        if (handler == null || json == null) return;
        try {
            JSONObject obj = new JSONObject(json);
            String type = obj.optString("type", "");
            String payload = obj.isNull("payload") ? null : obj.optString("payload", null);
            sMainHandler.post(() -> handler.onMessage(type, payload));
        } catch (Exception e) {
            android.util.Log.w("NativeExports", "onHostMessage: failed to parse JSON: " + e.getMessage());
        }
    }

    // ==================== Ads ====================
    //
    // The runtime owns no ad SDK. Everything below is transport: commands in,
    // events out. In particular the reward verdict for incentivised video is
    // whatever the host's AdHandler reports -- see AdEventSink's javadoc for
    // why nothing here may invent one.

    /**
     * Set or clear the ad handler for a session.
     *
     * @hide Called by {@link com.migo.runtime.GameSession#setAdHandler(AdHandler)}.
     */
    public static void setAdHandler(int sessionId, AdHandler handler) {
        if (handler == null) {
            sAdHandlers.remove(sessionId);
        } else {
            sAdHandlers.put(sessionId, handler);
        }
    }

    private static void clearAdHandler(int sessionId) {
        sAdHandlers.remove(sessionId);
    }

    /**
     * The sink handed to every AdHandler call.
     *
     * <p>One instance per session rather than per call: an ad SDK typically
     * retains the listener it was given for the life of the ad, so a per-call
     * sink would either leak or go stale. It is stateless apart from the
     * session id, so sharing it is safe from any thread.
     */
    private static final class SessionAdEventSink implements AdEventSink {
        private final int sessionId;

        SessionAdEventSink(int sessionId) {
            this.sessionId = sessionId;
        }

        private void emit(int adId, String event, JSONObject extra) {
            if (isSessionTerminated(sessionId)) return;
            try {
                JSONObject payload = extra != null ? extra : new JSONObject();
                payload.put("adId", adId);
                payload.put("event", event);
                NativeMethods.onAdEvent(sessionId, payload.toString());
            } catch (JSONException e) {
                android.util.Log.w("NativeExports",
                        "onAdEvent: failed to build payload: " + e.getMessage());
            }
        }

        @Override
        public void emitLoad(int adId) {
            emit(adId, "load", null);
        }

        @Override
        public void emitLoad(int adId, boolean useFallbackSharePage) {
            try {
                JSONObject extra = new JSONObject();
                extra.put("useFallbackSharePage", useFallbackSharePage);
                emit(adId, "load", extra);
            } catch (JSONException e) {
                emit(adId, "load", null);
            }
        }

        @Override
        public void emitError(int adId, int errCode, String errMsg) {
            try {
                JSONObject extra = new JSONObject();
                extra.put("errCode", errCode);
                extra.put("errMsg", errMsg != null ? errMsg : "ad error");
                emit(adId, "error", extra);
            } catch (JSONException e) {
                emit(adId, "error", null);
            }
        }

        @Override
        public void emitClose(int adId, boolean isEnded) {
            try {
                JSONObject extra = new JSONObject();
                // The reward verdict. Passed through from the host's ad SDK;
                // nothing in this file decides it.
                extra.put("isEnded", isEnded);
                emit(adId, "close", extra);
            } catch (JSONException e) {
                // A close that loses its verdict must not read as a completed
                // view: fall back to the un-rewarded shape rather than to a
                // payload with no isEnded at all.
                emit(adId, "close", null);
            }
        }

        @Override
        public void emitResize(int adId, int width, int height) {
            try {
                JSONObject extra = new JSONObject();
                extra.put("width", width);
                extra.put("height", height);
                emit(adId, "resize", extra);
            } catch (JSONException e) {
                emit(adId, "resize", null);
            }
        }

        @Override
        public void emitHide(int adId) {
            emit(adId, "hide", null);
        }
    }

    /** Per-session sinks, created on first use and dropped with the session. */
    private static final ConcurrentHashMap<Integer, SessionAdEventSink> sAdSinks =
            new ConcurrentHashMap<>();

    private static AdEventSink adSink(int sessionId) {
        SessionAdEventSink existing = sAdSinks.get(sessionId);
        if (existing != null) return existing;
        SessionAdEventSink created = new SessionAdEventSink(sessionId);
        SessionAdEventSink raced = sAdSinks.putIfAbsent(sessionId, created);
        return raced != null ? raced : created;
    }

    /**
     * The ad commands the runtime can send, each with what content is owed when
     * no advert can be shown.
     *
     * <p>{@link #settleWithoutAdvert} is abstract on purpose. Three of these
     * commands used to resolve the handler with a bare map lookup and
     * {@code return}, and content that had called {@code hide()} waited for an
     * {@code onHide} that never came. An abstract method makes a seventh ad
     * command unable to compile until somebody decides what it settles as, which
     * a shared default would have quietly answered for them.
     *
     * <p>Nothing here may touch the enclosing class's state. {@code NativeExports}
     * holds {@code android.os.Handler} statics, so initialising it needs a device;
     * this enum stays reachable from a host-JVM unit test only while it depends on
     * the sink interface alone.
     */
    enum AdOp {
        CREATE("createAd") {
            @Override
            void settleWithoutAdvert(AdEventSink sink, int adId, String reason) {
                sink.emitError(adId, -1, "createAd:fail " + reason);
            }
        },
        LOAD("loadAd") {
            @Override
            void settleWithoutAdvert(AdEventSink sink, int adId, String reason) {
                sink.emitError(adId, -1, "loadAd:fail " + reason);
            }
        },
        SHOW("showAd") {
            @Override
            void settleWithoutAdvert(AdEventSink sink, int adId, String reason) {
                // The close is not decoration: content following the common mini-game platform's idiom
                // decides its payout and resumes gameplay in onClose. It carries
                // isEnded = false, so reporting that no advert was shown mints
                // nothing.
                sink.emitShowFailed(adId, -1, "showAd:fail " + reason);
            }
        },
        HIDE("hideAd") {
            @Override
            void settleWithoutAdvert(AdEventSink sink, int adId, String reason) {
                // Nothing was ever displayed, so the ad is hidden. Reporting an
                // error instead would contradict AdHandler#hideAd, whose default
                // treats hiding an unsupported format as a no-op rather than a
                // failure.
                sink.emitHide(adId);
            }
        },
        UPDATE_STYLE("updateAdStyle") {
            @Override
            void settleWithoutAdvert(AdEventSink sink, int adId, String reason) {
                // A layout write owes content no event, with a host or without
                // one: the common mini-game platform has no callback for it.
            }
        },
        DESTROY("destroyAd") {
            @Override
            void settleWithoutAdvert(AdEventSink sink, int adId, String reason) {
                // Release is terminal. Content is not waiting, and the contract
                // is that nothing is emitted for this adId afterwards.
            }
        };

        private final String api;

        AdOp(String api) {
            this.api = api;
        }

        /** The api name as it appears in an ad error message. */
        String api() {
            return api;
        }

        /**
         * Report whatever content is waiting for when this command cannot reach
         * an advert.
         *
         * @param reason why, for the error message; ignored by commands that owe
         *               content no event
         */
        abstract void settleWithoutAdvert(AdEventSink sink, int adId, String reason);
    }

    /**
     * Resolve the handler for a session, settling the request on the ad's own
     * channel when there is none.
     *
     * <p>Settling rather than staying silent matters: content that called
     * show() is waiting for either a close or an error, and a silent drop leaves
     * it waiting forever. Registration is live session state a host may change,
     * so this is answered per call rather than once per isolate -- a host that
     * registers its handler a moment late must not lose every ad already
     * constructed.
     */
    private static AdHandler adHandlerOrSettle(int sessionId, int adId, AdOp op) {
        RuntimeContext ctx = RuntimeRegistry.get(sessionId);
        if (ctx == null) {
            clearAdHandler(sessionId);
            sAdSinks.remove(sessionId);
            return null;
        }
        AdHandler handler = sAdHandlers.get(sessionId);
        if (handler == null) {
            op.settleWithoutAdvert(adSink(sessionId), adId, "no ad handler");
            return null;
        }
        return handler;
    }

    /**
     * Report a handler that threw, then settle what content was waiting for.
     *
     * <p>A host exception is a host bug, but content cannot be left holding a
     * pending request because of it.
     */
    private static void adHandlerThrew(int sessionId, int adId, AdOp op, Exception e) {
        android.util.Log.w("NativeExports", op.api() + ": handler threw: " + e);
        op.settleWithoutAdvert(adSink(sessionId), adId, "handler threw: " + e);
    }

    private static JSONObject parseAdRequest(String requestJson) {
        if (requestJson == null || requestJson.isEmpty()) {
            return new JSONObject();
        }
        try {
            return new JSONObject(requestJson);
        } catch (JSONException e) {
            return new JSONObject();
        }
    }

    private static int parseAdId(JSONObject request) {
        return request != null ? request.optInt("adId", 0) : 0;
    }

    /** @hide Called from native when content creates an ad. */
    public static void adCreate(int sessionId, String requestJson) {
        JSONObject request = parseAdRequest(requestJson);
        int adId = parseAdId(request);
        if (adId == 0) return;

        AdHandler handler = adHandlerOrSettle(sessionId, adId, AdOp.CREATE);
        if (handler == null) return;

        String adType = request.optString("adType", "");
        String adUnitId = request.optString("adUnitId", "");
        JSONObject options = request.optJSONObject("options");
        String optionsJson = options != null ? options.toString() : "{}";

        try {
            handler.createAd(adId, adType, adUnitId, optionsJson, adSink(sessionId));
        } catch (Exception e) {
            adHandlerThrew(sessionId, adId, AdOp.CREATE, e);
        }
    }

    /** @hide Called from native when content loads an ad. */
    public static void adLoad(int sessionId, String requestJson) {
        JSONObject request = parseAdRequest(requestJson);
        int adId = parseAdId(request);
        if (adId == 0) return;

        AdHandler handler = adHandlerOrSettle(sessionId, adId, AdOp.LOAD);
        if (handler == null) return;

        try {
            handler.loadAd(adId, adSink(sessionId));
        } catch (Exception e) {
            adHandlerThrew(sessionId, adId, AdOp.LOAD, e);
        }
    }

    /** @hide Called from native when content shows an ad. */
    public static void adShow(int sessionId, String requestJson) {
        JSONObject request = parseAdRequest(requestJson);
        int adId = parseAdId(request);
        if (adId == 0) return;

        AdHandler handler = adHandlerOrSettle(sessionId, adId, AdOp.SHOW);
        if (handler == null) return;

        try {
            handler.showAd(adId, adSink(sessionId));
        } catch (Exception e) {
            adHandlerThrew(sessionId, adId, AdOp.SHOW, e);
        }
    }

    /** @hide Called from native when content hides an ad. */
    public static void adHide(int sessionId, String requestJson) {
        JSONObject request = parseAdRequest(requestJson);
        int adId = parseAdId(request);
        if (adId == 0) return;

        AdHandler handler = adHandlerOrSettle(sessionId, adId, AdOp.HIDE);
        if (handler == null) return;

        try {
            handler.hideAd(adId, adSink(sessionId));
        } catch (Exception e) {
            adHandlerThrew(sessionId, adId, AdOp.HIDE, e);
        }
    }

    /** @hide Called from native when content mutates a positioned ad's style. */
    public static void adUpdateStyle(int sessionId, String requestJson) {
        JSONObject request = parseAdRequest(requestJson);
        int adId = parseAdId(request);
        if (adId == 0) return;

        AdHandler handler = adHandlerOrSettle(sessionId, adId, AdOp.UPDATE_STYLE);
        if (handler == null) return;

        JSONObject style = request.optJSONObject("style");
        String styleJson = style != null ? style.toString() : "{}";

        try {
            handler.updateAdStyle(adId, styleJson, adSink(sessionId));
        } catch (Exception e) {
            adHandlerThrew(sessionId, adId, AdOp.UPDATE_STYLE, e);
        }
    }

    /** @hide Called from native when content destroys an ad. */
    public static void adDestroy(int sessionId, String requestJson) {
        JSONObject request = parseAdRequest(requestJson);
        int adId = parseAdId(request);
        if (adId == 0) return;

        AdHandler handler = adHandlerOrSettle(sessionId, adId, AdOp.DESTROY);
        if (handler == null) return;

        try {
            handler.destroyAd(adId);
        } catch (Exception e) {
            adHandlerThrew(sessionId, adId, AdOp.DESTROY, e);
        }
    }

    // ==================== Permission ====================
    //
    // The host decides; this is transport. Standing decisions go straight into
    // the native cache that `require_scope` reads, and one-off `migo.authorize()`
    // replies settle the pending promise. Nothing is kept in JavaScript: a
    // permission answer content can reach is a permission answer content can
    // change.

    /**
     * Set or clear the permission handler for a session.
     *
     * @hide Called by {@link com.migo.runtime.GameSession#setPermissionHandler}.
     */
    public static void setPermissionHandler(int sessionId, PermissionHandler handler) {
        if (handler == null) {
            sPermissionHandlers.remove(sessionId);
        } else {
            sPermissionHandlers.put(sessionId, handler);
        }
    }

    private static void clearPermissionHandler(int sessionId) {
        sPermissionHandlers.remove(sessionId);
    }

    /** One sink per session; stateless apart from the id, so sharing is safe. */
    static final class SessionPermissionSink implements PermissionSink {
        interface FailureReporter {
            void report(String scope, boolean granted, RuntimeException failure);
        }

        interface CloseScheduler {
            boolean schedule();
        }

        private final int sessionId;
        private final PermissionOperationGate operations;
        private final BooleanSupplier sessionTerminated;
        private final FailureReporter failureReporter;
        private final CloseScheduler closeScheduler;

        SessionPermissionSink(int sessionId) {
            this(
                    sessionId,
                    sPermissionOperations,
                    () -> isSessionTerminated(sessionId),
                    (scope, granted, failure) -> {
                        android.util.Log.e(TAG, "permission_update_failed session=" + sessionId
                                + " scope=" + scope + " granted=" + granted, failure);
                        onError(sessionId, ErrorCode.ERR_CLEANUP_FAILED,
                                "permission update cleanup failed", failure.toString());
                    },
                    () -> scheduleTerminalClose(sessionId));
        }

        SessionPermissionSink(
                int sessionId,
                PermissionOperationGate operations,
                BooleanSupplier sessionTerminated,
                FailureReporter failureReporter,
                CloseScheduler closeScheduler) {
            this.sessionId = sessionId;
            this.operations = operations;
            this.sessionTerminated = sessionTerminated;
            this.failureReporter = failureReporter;
            this.closeScheduler = closeScheduler;
        }

        @Override
        public void setScope(String scope, boolean granted) {
            if (scope == null || scope.isEmpty()) return;
            if (sessionTerminated.getAsBoolean()) return;
            PermissionOperationGate.Result result = operations.update(
                    sessionId,
                    scope,
                    granted,
                    () -> PermissionRevocation.update(
                            scope,
                            sessionTerminated,
                            () -> NativeMethods.updatePermission(sessionId, scope, granted)));
            RuntimeException failure = result.failure();
            if (failure == null) return;
            try {
                failureReporter.report(scope, granted, failure);
            } catch (RuntimeException reportFailure) {
                failure.addSuppressed(reportFailure);
            }
            try {
                if (!closeScheduler.schedule()) {
                    failure.addSuppressed(new IllegalStateException(
                            "failed to schedule terminal close"));
                }
            } catch (RuntimeException scheduleFailure) {
                failure.addSuppressed(scheduleFailure);
            }
            throw failure;
        }

        @Override
        public void resolveRequest(int requestId, boolean granted) {
            if (isSessionTerminated(sessionId)) return;
            try {
                JSONObject res = new JSONObject();
                res.put("requestId", requestId);
                res.put("granted", granted);
                NativeMethods.onAuthorizeResult(sessionId, res.toString());
            } catch (JSONException e) {
                failRequest(requestId, "internal error");
            }
        }

        @Override
        public void failRequest(int requestId, String errMsg) {
            if (isSessionTerminated(sessionId)) return;
            try {
                JSONObject res = new JSONObject();
                res.put("requestId", requestId);
                res.put("error", errMsg != null ? errMsg : "authorize failed");
                NativeMethods.onAuthorizeResult(sessionId, res.toString());
            } catch (JSONException e) {
                NativeMethods.onAuthorizeResult(sessionId,
                        "{\"requestId\":" + requestId + ",\"error\":\"internal error\"}");
            }
        }
    }

    /** Targeted teardown invoked synchronously by native permission revocation. */
    public static void revokePermissionResources(int sessionId, String scope) {
        try {
            ResourceCleanup.runAll(
                    () -> {
                        PermissionOperationGate.Result result =
                                sPermissionOperations.revoke(sessionId, scope);
                        if (result.failure() != null) throw result.failure();
                    },
                    () -> PermissionRevocation.tearDown(
                            sessionId, scope, sPermissionResources,
                            () -> requireTerminalCloseScheduled(sessionId)));
        } catch (RuntimeException cleanupFailure) {
            if (!scheduleTerminalClose(sessionId)) {
                cleanupFailure.addSuppressed(new IllegalStateException(
                        "failed to schedule terminal close"));
            }
            throw cleanupFailure;
        }
    }

    private static boolean scheduleTerminalClose(int sessionId) {
        GameSession session = sSessions.get(sessionId);
        return sTerminalCloses.schedule(session, sMainHandler::post, GameSession::close);
    }

    /** Reports asynchronous resource cleanup failure without throwing on a Looper callback. */
    public static void reportCleanupFailureAndScheduleTerminalClose(
            int sessionId,
            String operation,
            RuntimeException failure) {
        android.util.Log.e(TAG, operation + " failed session=" + sessionId, failure);
        try {
            onError(sessionId, ErrorCode.ERR_CLEANUP_FAILED,
                    operation + " failed", failure.toString());
        } catch (RuntimeException reportFailure) {
            failure.addSuppressed(reportFailure);
        }
        try {
            if (!scheduleTerminalClose(sessionId)) {
                failure.addSuppressed(new IllegalStateException(
                        "failed to schedule terminal close"));
            }
        } catch (RuntimeException scheduleFailure) {
            failure.addSuppressed(scheduleFailure);
        }
    }

    private static void requireTerminalCloseScheduled(int sessionId) {
        if (!scheduleTerminalClose(sessionId)) {
            throw new IllegalStateException(
                    "failed to schedule session termination after permission cleanup");
        }
    }

    private static final ConcurrentHashMap<Integer, SessionPermissionSink> sPermissionSinks =
            new ConcurrentHashMap<>();

    private static PermissionSink permissionSink(int sessionId) {
        SessionPermissionSink existing = sPermissionSinks.get(sessionId);
        if (existing != null) return existing;
        SessionPermissionSink created = new SessionPermissionSink(sessionId);
        SessionPermissionSink raced = sPermissionSinks.putIfAbsent(sessionId, created);
        return raced != null ? raced : created;
    }

    /** @hide Called from native when content calls migo.authorize(). */
    public static void permissionRequest(int sessionId, String requestJson) {
        int requestId = 0;
        String scope = "";
        String desc = "";
        try {
            JSONObject req = new JSONObject(requestJson == null ? "{}" : requestJson);
            requestId = req.optInt("requestId", 0);
            scope = req.optString("scope", "");
            desc = req.optString("desc", "");
        } catch (JSONException ignored) {
            // Fall through: a malformed request still owes content an answer.
        }

        RuntimeContext ctx = RuntimeRegistry.get(sessionId);
        if (ctx == null) {
            clearPermissionHandler(sessionId);
            sPermissionSinks.remove(sessionId);
            return;
        }

        PermissionHandler handler = sPermissionHandlers.get(sessionId);
        if (handler == null) {
            // No handler is a refusal, not silence: content is awaiting a reply
            // and would otherwise wait forever.
            permissionSink(sessionId).failRequest(requestId, "authorize:fail no permission handler");
            return;
        }
        if (scope.isEmpty()) {
            permissionSink(sessionId).failRequest(requestId, "authorize:fail scope is required");
            return;
        }

        try {
            handler.requestScope(requestId, scope, desc, permissionSink(sessionId));
        } catch (Exception e) {
            permissionSink(sessionId).failRequest(requestId, "authorize:fail " + e);
        }
    }

}
