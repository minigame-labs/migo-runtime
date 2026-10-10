package com.migo.runtime.internal.platform;

import android.annotation.SuppressLint;
import android.app.Activity;
import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothGatt;
import android.bluetooth.BluetoothGattCallback;
import android.bluetooth.BluetoothGattCharacteristic;
import android.bluetooth.BluetoothGattDescriptor;
import android.bluetooth.BluetoothGattService;
import android.bluetooth.BluetoothProfile;
import android.bluetooth.le.BluetoothLeScanner;
import android.bluetooth.le.ScanCallback;
import android.bluetooth.le.ScanFilter;
import android.bluetooth.le.ScanResult;
import android.bluetooth.le.ScanSettings;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.bluetooth.le.ScanRecord;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.os.ParcelUuid;
import android.util.Log;
import android.util.SparseArray;

import com.migo.runtime.internal.CallbackCorrelation;
import com.migo.runtime.internal.ExclusiveDeviceArbiter;
import com.migo.runtime.internal.NativeExports;
import com.migo.runtime.internal.NativeMethods;
import com.migo.runtime.internal.ResourceCleanup;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.lang.ref.WeakReference;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.function.BooleanSupplier;

/**
 * Manages Bluetooth adapter, BLE device discovery, pairing, and Beacon operations.
 * One instance per session.
 *
 * <p>Every operation is a request answered exactly once through its {@link Reply}:
 * when the operation has happened -- a connection made, a write acknowledged, an
 * RSSI read -- not when it was issued. Failures carry the common mini-game
 * platform's Bluetooth codes ({@link BluetoothFailure}).
 */
public class BluetoothManager {

    private static final String TAG = "BluetoothManager";

    /**
     * The Bluetooth requests, in the order the host-service contract numbers them
     * ({@code contracts/runtime/host-services.json}, service {@code bluetooth}):
     * an ordinal is the method number the runtime routes the answer by.
     */
    public enum Method {
        OPEN_ADAPTER("openBluetoothAdapter"),
        CLOSE_ADAPTER("closeBluetoothAdapter"),
        GET_ADAPTER_STATE("getBluetoothAdapterState"),
        START_DEVICES_DISCOVERY("startBluetoothDevicesDiscovery"),
        STOP_DEVICES_DISCOVERY("stopBluetoothDevicesDiscovery"),
        GET_DEVICES("getBluetoothDevices"),
        GET_CONNECTED_DEVICES("getConnectedBluetoothDevices"),
        MAKE_PAIR("makeBluetoothPair"),
        IS_DEVICE_PAIRED("isBluetoothDevicePaired"),
        START_BEACON_DISCOVERY("startBeaconDiscovery"),
        STOP_BEACON_DISCOVERY("stopBeaconDiscovery"),
        GET_BEACONS("getBeacons"),
        CREATE_BLE_CONNECTION("createBLEConnection"),
        CLOSE_BLE_CONNECTION("closeBLEConnection"),
        GET_BLE_DEVICE_SERVICES("getBLEDeviceServices"),
        GET_BLE_DEVICE_CHARACTERISTICS("getBLEDeviceCharacteristics"),
        READ_BLE_CHARACTERISTIC_VALUE("readBLECharacteristicValue"),
        WRITE_BLE_CHARACTERISTIC_VALUE("writeBLECharacteristicValue"),
        NOTIFY_BLE_CHARACTERISTIC_VALUE_CHANGE("notifyBLECharacteristicValueChange"),
        GET_BLE_DEVICE_RSSI("getBLEDeviceRSSI"),
        SET_BLE_MTU("setBLEMTU"),
        GET_BLE_MTU("getBLEMTU");

        public final String apiName;

        Method(String apiName) {
            this.apiName = apiName;
        }
    }

    /** Where a request's answer goes: the runtime's Bluetooth result callback. */
    public interface ResultSink {
        void deliver(int sessionId, int method, String resultJson);
    }

    /**
     * The answer to one request, given once. A request answered twice would
     * settle content's call with whichever arrived first and drop the other
     * silently; refusing the second makes a double answer impossible instead.
     */
    public static final class Reply {
        private final int sessionId;
        private final Method method;
        private final int requestId;
        private final ResultSink sink;
        private final AtomicBoolean answered = new AtomicBoolean(false);

        public Reply(int sessionId, Method method, int requestId, ResultSink sink) {
            this.sessionId = sessionId;
            this.method = method;
            this.requestId = requestId;
            this.sink = sink;
        }

        public void ok(JSONObject result) {
            if (!answered.compareAndSet(false, true)) return;
            JSONObject answer = result != null ? result : new JSONObject();
            try {
                CallbackCorrelation.stamp(answer, requestId);
            } catch (JSONException impossible) {
                // stamp only puts an int.
            }
            sink.deliver(sessionId, method.ordinal(), answer.toString());
        }

        public void ok() {
            ok(null);
        }

        public void fail(int errCode, String reason) {
            if (!answered.compareAndSet(false, true)) return;
            JSONObject answer = new JSONObject();
            try {
                answer.put("error", method.apiName + ":fail " + (reason != null ? reason : "system error"));
                answer.put("errCode", errCode);
                CallbackCorrelation.stamp(answer, requestId);
            } catch (JSONException impossible) {
                // Only strings and ints are put.
            }
            sink.deliver(sessionId, method.ordinal(), answer.toString());
        }

        public boolean isAnswered() {
            return answered.get();
        }
    }

    /** A failure with the platform's Bluetooth code. */
    public static final class BluetoothFailure extends RuntimeException {
        public final int errCode;

        public BluetoothFailure(int errCode, String reason) {
            super(reason);
            this.errCode = errCode;
        }
    }

    public static final int ALREADY_CONNECTED = -1;
    public static final int NOT_INIT = 10000;
    public static final int NOT_AVAILABLE = 10001;
    public static final int NO_DEVICE = 10002;
    public static final int CONNECTION_FAIL = 10003;
    public static final int NO_SERVICE = 10004;
    public static final int NO_CHARACTERISTIC = 10005;
    public static final int NO_CONNECTION = 10006;
    public static final int PROPERTY_NOT_SUPPORT = 10007;
    public static final int SYSTEM_ERROR = 10008;
    public static final int OPERATE_TIME_OUT = 10012;
    public static final int INVALID_DATA = 10013;
    public static final int BEACON_UNAVAILABLE = 11001;

    private final int sessionId;
    private final WeakReference<Activity> activityRef;
    private final BluetoothAdapter adapter;
    private final CleanupFailureReporter cleanupFailureReporter;
    private final ConnectionStateReporter connectionStateReporter;
    private final GattCallbackAdmission gattCallbackAdmission;
    private final BooleanSupplier gattConnectPermissionGranted;
    private final BooleanSupplier gattSessionTerminated;
    private final GattEventReporter gattEventReporter;
    private final BeaconUpdateReporter beaconUpdateReporter;

    interface CleanupFailureReporter {
        void report(String operation, RuntimeException failure);
    }

    interface ConnectionStateReporter {
        void report(String deviceId, boolean connected);
    }

    interface GattCallbackAdmission {
        boolean run(BooleanSupplier callback);
    }

    interface GattEventReporter {
        void characteristic(
                String deviceId,
                String serviceId,
                String characteristicId,
                byte[] value);
        void mtu(String deviceId, int mtu);
    }

    /**
     * Receives the JSON-encoded full beacon list on each scan update.
     *
     * <p>Separated from {@link NativeMethods#onBeaconUpdate} for the same
     * reason {@link GattEventReporter} is separated from the GATT notification
     * path: native JNI is not available in host JVM tests, so production
     * code that needs to be unit-tested routes its delivery through this
     * interface instead of calling the bridge directly.
     */
    interface BeaconUpdateReporter {
        void update(String beaconsJson);
    }

    interface GattConnection {
        BluetoothGatt raw();
        boolean discoverServices();
        void disconnect();
        void close();
    }

    private static final class AndroidGattConnection implements GattConnection {
        private final BluetoothGatt gatt;

        AndroidGattConnection(BluetoothGatt gatt) {
            this.gatt = gatt;
        }

        @Override public BluetoothGatt raw() {
            return gatt;
        }

        /* Permission admission is performed by BluetoothManager immediately
         * before this test seam is invoked. Framework calls still throw on a
         * revoke race, and every caller catches that RuntimeException. */
        @SuppressLint("MissingPermission")
        @Override public boolean discoverServices() {
            return gatt.discoverServices();
        }

        @SuppressLint("MissingPermission")
        @Override public void disconnect() {
            gatt.disconnect();
        }

        @SuppressLint("MissingPermission")
        @Override public void close() {
            gatt.close();
        }
    }

    /**
     * The admission supplier and the delivery runnable for one GATT attempt,
     * reused across notifications instead of captured afresh on each one.
     *
     * <p>Per-event paths must not allocate: a characteristic notification
     * arrives at whatever rate the peripheral chooses. Written closure-style
     * the dispatch built two capturing lambdas every time — one for the
     * admission gate and one for the delivery — and neither is a lambda that
     * can be non-capturing, because both need the event's own values. Carrying
     * those values in fields of one long-lived object is what removes the
     * allocation; implementing both interfaces on it is what makes it one
     * object rather than two.
     *
     * <p><b>Every use holds this object's own monitor</b> ({@code fill} through
     * the end of the dispatch it feeds), because the fields are scratch space
     * shared by whichever thread the platform delivers on. Two notifications
     * interleaving here would deliver one characteristic's value under another's
     * identifier, which is worse than the allocation being removed. The lock
     * order is this monitor, then the permission session's, then the attempt's,
     * and it is the only order in which those three are ever taken: nothing
     * reaches a dispatch carrier while holding either of the others.
     */
    private static final class CharacteristicDispatch implements BooleanSupplier, Runnable {
        private final BluetoothManager manager;
        private final GattAttempt attempt;
        private String deviceId;
        private GattConnection connection;
        private String serviceId;
        private String characteristicId;
        private byte[] value;

        CharacteristicDispatch(BluetoothManager manager, GattAttempt attempt) {
            this.manager = manager;
            this.attempt = attempt;
        }

        /** Load one event. The caller holds this object's monitor. */
        void fill(
                String deviceId,
                GattConnection connection,
                String serviceId,
                String characteristicId,
                byte[] value) {
            this.deviceId = deviceId;
            this.connection = connection;
            this.serviceId = serviceId;
            this.characteristicId = characteristicId;
            this.value = value;
        }

        /**
         * Release the event's references once it has been delivered.
         *
         * <p>Not hygiene: the carrier outlives the connection, so a retained
         * {@link GattConnection} would keep a closed {@code BluetoothGatt}
         * reachable until the next notification, and there may not be one.
         */
        void clear() {
            deviceId = null;
            connection = null;
            serviceId = null;
            characteristicId = null;
            value = null;
        }

        synchronized boolean isEmpty() {
            return deviceId == null && connection == null && serviceId == null
                    && characteristicId == null && value == null;
        }

        @Override public boolean getAsBoolean() {
            return manager.gattConnections.get(deviceId) == attempt
                    && attempt.dispatchIfActive(
                            connection,
                            manager.gattConnectPermissionGranted,
                            manager.gattSessionTerminated,
                            this);
        }

        @Override public void run() {
            manager.gattEventReporter.characteristic(
                    deviceId, serviceId, characteristicId, value);
        }
    }

    static final class GattAttempt {
        private GattConnection connection;
        private boolean acceptingCallbacks = true;
        /** The createBLEConnection waiting for this attempt to connect. */
        private Reply connectReply;
        /** Requests waiting on this connection's GATT callbacks. */
        final ConcurrentHashMap<String, Reply> pendingWrites = new ConcurrentHashMap<>();
        final ConcurrentHashMap<String, Reply> pendingDescriptorWrites = new ConcurrentHashMap<>();
        final List<Reply> pendingRssi = new ArrayList<>();
        Reply pendingMtu;

        synchronized void awaitConnection(Reply reply) {
            connectReply = reply;
        }

        synchronized Reply takeConnectReply() {
            Reply reply = connectReply;
            connectReply = null;
            return reply;
        }

        /** Fail everything still waiting on this connection, which is going. */
        void failPending(int errCode, String reason) {
            Reply connect = takeConnectReply();
            if (connect != null) connect.fail(errCode, reason);
            for (Reply reply : pendingWrites.values()) reply.fail(errCode, reason);
            pendingWrites.clear();
            for (Reply reply : pendingDescriptorWrites.values()) reply.fail(errCode, reason);
            pendingDescriptorWrites.clear();
            synchronized (this) {
                for (Reply reply : pendingRssi) reply.fail(errCode, reason);
                pendingRssi.clear();
                if (pendingMtu != null) pendingMtu.fail(errCode, reason);
                pendingMtu = null;
            }
        }
        /**
         * Created with the attempt, on the cold connect path, so no notification
         * ever pays for it. Shared by the read and notification paths, which its
         * own monitor serialises against each other as well as against
         * themselves.
         */
        private CharacteristicDispatch dispatch;

        synchronized boolean attach(GattConnection candidate) {
            if (!acceptingCallbacks) return false;
            if (connection == null) connection = candidate;
            return matches(candidate);
        }

        synchronized GattConnection connection() {
            return connection;
        }

        synchronized GattConnection beginClose() {
            acceptingCallbacks = false;
            return connection;
        }

        synchronized boolean dispatchIfActive(
                GattConnection candidate,
                BooleanSupplier connectPermissionGranted,
                BooleanSupplier sessionTerminated,
                Runnable callback) {
            if (!acceptingCallbacks || !matches(candidate)
                    || !connectPermissionGranted.getAsBoolean()
                    || sessionTerminated.getAsBoolean()) {
                return false;
            }
            callback.run();
            return true;
        }

        private boolean matches(GattConnection candidate) {
            if (connection == candidate) return true;
            BluetoothGatt raw = connection != null ? connection.raw() : null;
            return raw != null && raw == candidate.raw();
        }

        /**
         * The carrier for this attempt, created on first use.
         *
         * <p>Created here rather than in the constructor because most attempts
         * never carry a characteristic, and there is no notification cheap
         * enough to be worth the object for a connection that only ever reports
         * its state.
         *
         * <p><b>This returns before the caller takes the carrier's monitor</b>,
         * so the attempt's monitor is never held while acquiring the carrier's.
         * That is what keeps the one order the dispatch path uses -- carrier,
         * then permission session, then attempt -- from being inverted here.
         */
        private synchronized CharacteristicDispatch dispatch(BluetoothManager manager) {
            if (dispatch == null) dispatch = new CharacteristicDispatch(manager, this);
            return dispatch;
        }

        /** Whether the carrier is holding an event's references. */
        synchronized boolean carrierIsEmptyForTests() {
            return dispatch == null || dispatch.isEmpty();
        }
    }

    private boolean adapterOpened = false;
    private volatile boolean discovering = false;

    /** Discovered devices keyed by address. */
    private final ConcurrentHashMap<String, JSONObject> discoveredDevices = new ConcurrentHashMap<>();

    /** Active GATT connections keyed by device address. */
    private final ConcurrentHashMap<String, GattAttempt> gattConnections =
            new ConcurrentHashMap<>();

    /**
     * Candidate handles whose {@code close()} threw. A late candidate is never in
     * {@link #gattConnections} -- that map holds the attempt that won -- so a failed
     * close has no map entry to keep it alive the way a failed owned close does.
     * Without this the {@code BluetoothGatt} would simply be dropped: the OS handle
     * stays open for process life and nothing ever tries again.
     *
     * <p>Entries leave only when a close succeeds, which is the same
     * retain-on-failure rule {@code closeAndRemoveGatt} gets by not reaching its
     * {@code remove} when {@code closeGatt} throws.
     */
    private final Set<GattConnection> unclosedCandidates = ConcurrentHashMap.newKeySet();

    /**
     * Orders every connection-state decision against its own delivery.
     *
     * <p><b>The bug this exists for.</b> Deciding "does this attempt still speak
     * for the device?" and delivering the answer were two steps. A retired
     * attempt could read "no owner", be descheduled, and deliver its
     * {@code false} after a replacement had already published and reported
     * {@code true} -- leaving content permanently told the device is
     * disconnected while it is connected, until some later event happens to
     * correct it. The decision was right when it was made; nothing kept it right
     * until it arrived.
     *
     * <p><b>Why one monitor per session rather than one per device.</b> Ordering
     * is only required between reports for the same device, so a per-device
     * monitor is the narrowest correct answer -- and it needs a lifetime longer
     * than the attempts it orders, because the whole point is ordering an
     * outgoing attempt against its replacement. That means a map of monitors
     * outliving the entries they guard, with its own eviction rule and its own
     * bound, since content chooses the device ids. A single monitor is strictly
     * stronger, has no lifecycle, and costs two devices' connect events the time
     * of one queue push -- on a path that fires when a peripheral connects or
     * drops, not per notification.
     *
     * <p><b>What is deliberately outside it.</b> {@code close()},
     * {@code disconnect()} and {@code discoverServices()} are framework calls
     * that can block, and the map mutations they accompany. Only the ownership
     * re-check and the report itself are inside, which is sufficient: a
     * publisher's map write happens before its own report in program order, and
     * the monitor orders the reports, so a reader that acquires the monitor
     * after that write sees it. Holding it across the report is safe for a
     * reason worth stating rather than assuming -- the report is a post, not a
     * wait: it enqueues on a bounded channel and returns, never re-enters Java
     * and never waits on a Migo lock, which is exactly the property the
     * permission gate's counted lease exists to preserve elsewhere.
     */
    private final Object connectionStateOrder = new Object();

    /**
     * The value of a characteristic that reported none.
     *
     * <p>Shared because a zero-length array is immutable in every way that
     * matters and {@code new byte[0]} on a notification path is an allocation
     * for nothing.
     */
    private static final byte[] NO_VALUE = new byte[0];

    /**
     * How many distinct UUID strings one session will cache.
     *
     * <p>A device's GATT database is small -- tens of attributes -- so a real
     * peripheral never approaches this. The bound is for one that misbehaves:
     * the cache is fed by identifiers the remote end chooses, and an unbounded
     * map fed by a remote party is a memory pressure the peripheral controls.
     * Past the bound the text is still produced, just not kept.
     */
    private static final int UUID_TEXT_CACHE_LIMIT = 256;

    /**
     * Maximum number of distinct remote beacons retained in one scan session.
     *
     * <p>Beacon identifiers come from the remote environment. Without a bound,
     * a long-running scan retains every identifier it has ever observed and
     * rebuilds an ever-larger JSON array for each later advertisement.
     */
    static final int BEACON_CACHE_LIMIT = 256;

    /**
     * Canonical text for the UUIDs this session has seen.
     *
     * <p>{@code UUID.toString} formats 36 characters every call, and a
     * notification needs two of them -- the service's and the characteristic's
     * -- for identifiers that are the same on every notification of one stream.
     * The notification path cannot allocate; a lookup keyed by the UUID object
     * the platform already holds removes that formatting without changing what
     * is delivered.
     */
    private final ConcurrentHashMap<UUID, String> uuidText = new ConcurrentHashMap<>();

    /** Cached negotiated MTU per device. Updated by onMtuChanged callback. */
    private final ConcurrentHashMap<String, Integer> negotiatedMtu = new ConcurrentHashMap<>();

    /** Cached RSSI per device. Updated by onReadRemoteRssi callback. */
    private final ConcurrentHashMap<String, Integer> cachedRssi = new ConcurrentHashMap<>();

    /** Client Characteristic Configuration Descriptor UUID for enabling notifications. */
    private static final UUID CCCD_UUID = UUID.fromString("00002902-0000-1000-8000-00805f9b34fb");

    private BluetoothLeScanner leScanner;
    private ScanCallback leScanCallback;
    private BroadcastReceiver adapterStateReceiver;
    private final LifecycleRequestState<String> discoveryRequest;
    private final LifecycleRequestState<String> beaconRequest;

    public BluetoothManager(int sessionId, Activity activity) {
        this(sessionId, activity, false);
    }

    public BluetoothManager(int sessionId, Activity activity, boolean lifecycleSuspended) {
        this.sessionId = sessionId;
        this.activityRef = new WeakReference<>(activity);
        this.adapter = getAdapter(activity);
        this.cleanupFailureReporter = (operation, failure) ->
                NativeExports.reportCleanupFailureAndScheduleTerminalClose(
                        sessionId, operation, failure);
        this.connectionStateReporter = (deviceId, connected) ->
                NativeMethods.onBLEConnectionStateChange(
                        sessionId, deviceId, connected);
        this.gattCallbackAdmission = callback -> NativeExports.runIfPermissionGranted(
                sessionId, "scope.bluetooth", callback);
        this.gattConnectPermissionGranted = this::hasConnectPermission;
        this.gattSessionTerminated = () -> NativeExports.isSessionTerminated(sessionId);
        this.gattEventReporter = new GattEventReporter() {
            @Override public void characteristic(
                    String deviceId,
                    String serviceId,
                    String characteristicId,
                    byte[] value) {
                NativeMethods.onBLECharacteristicValueChange(
                        sessionId, deviceId, serviceId, characteristicId, value);
            }

            @Override public void mtu(String deviceId, int mtu) {
                NativeMethods.onBLEMTUChange(sessionId, deviceId, mtu);
            }
        };
        this.beaconUpdateReporter =
                beaconsJson -> NativeMethods.onBeaconUpdate(sessionId, beaconsJson);
        this.discoveryRequest = new LifecycleRequestState<>(lifecycleSuspended);
        this.beaconRequest = new LifecycleRequestState<>(lifecycleSuspended);
    }

    BluetoothManager(
            int sessionId,
            CleanupFailureReporter cleanupFailureReporter,
            ConnectionStateReporter connectionStateReporter) {
        this(
                sessionId,
                cleanupFailureReporter,
                connectionStateReporter,
                callback -> callback.getAsBoolean(),
                () -> true,
                () -> false,
                new GattEventReporter() {
                    @Override public void characteristic(
                            String deviceId,
                            String serviceId,
                            String characteristicId,
                            byte[] value) {}

                    @Override public void mtu(String deviceId, int mtu) {}
                },
                beaconsJson -> {});
    }
    BluetoothManager(
            int sessionId,
            CleanupFailureReporter cleanupFailureReporter,
            ConnectionStateReporter connectionStateReporter,
            BooleanSupplier gattConnectPermissionGranted,
            BooleanSupplier gattSessionTerminated,
            GattEventReporter gattEventReporter) {
        this(
                sessionId,
                cleanupFailureReporter,
                connectionStateReporter,
                callback -> callback.getAsBoolean(),
                gattConnectPermissionGranted,
                gattSessionTerminated,
                gattEventReporter,
                beaconsJson -> {});
    }

    BluetoothManager(
            int sessionId,
            CleanupFailureReporter cleanupFailureReporter,
            ConnectionStateReporter connectionStateReporter,
            GattCallbackAdmission gattCallbackAdmission,
            BooleanSupplier gattConnectPermissionGranted,
            BooleanSupplier gattSessionTerminated,
            GattEventReporter gattEventReporter,
            BeaconUpdateReporter beaconUpdateReporter) {
        this.sessionId = sessionId;
        this.activityRef = new WeakReference<>(null);
        this.adapter = null;
        this.cleanupFailureReporter = cleanupFailureReporter;
        this.connectionStateReporter = connectionStateReporter;
        this.gattCallbackAdmission = gattCallbackAdmission;
        this.gattConnectPermissionGranted = gattConnectPermissionGranted;
        this.gattSessionTerminated = gattSessionTerminated;
        this.gattEventReporter = gattEventReporter;
        this.beaconUpdateReporter = beaconUpdateReporter;
        this.discoveryRequest = new LifecycleRequestState<>(false);
        this.beaconRequest = new LifecycleRequestState<>(false);
    }
    BluetoothManager(
            int sessionId,
            CleanupFailureReporter cleanupFailureReporter,
            ConnectionStateReporter connectionStateReporter,
            GattCallbackAdmission gattCallbackAdmission,
            BooleanSupplier gattConnectPermissionGranted,
            BooleanSupplier gattSessionTerminated,
            GattEventReporter gattEventReporter) {
        this(
                sessionId,
                cleanupFailureReporter,
                connectionStateReporter,
                gattCallbackAdmission,
                gattConnectPermissionGranted,
                gattSessionTerminated,
                gattEventReporter,
                beaconsJson -> {});
    }



    private Activity getActivity() {
        return activityRef.get();
    }

    private boolean hasConnectPermission() {
        Activity activity = getActivity();
        boolean granted = activity != null
                && Permissions.isGranted(activity, Permissions.BLUETOOTH_CONNECT);
        return BluetoothPermissionPolicy.canConnect(Build.VERSION.SDK_INT, granted);
    }

    private boolean hasScanPermission() {
        Activity activity = getActivity();
        boolean scanGranted = activity != null
                && Permissions.isGranted(activity, Permissions.BLUETOOTH_SCAN);
        boolean locationGranted = activity != null
                && Permissions.isGranted(activity, Permissions.FINE_LOCATION);
        return BluetoothPermissionPolicy.canScan(
                Build.VERSION.SDK_INT, scanGranted, locationGranted);
    }

    private void requireConnectPermission(String operation) {
        if (!hasConnectPermission()) {
            throw new SecurityException(
                    operation + ":fail permission denied (BLUETOOTH_CONNECT)");
        }
    }

    private void requireScanPermission(String operation) {
        if (!hasScanPermission()) {
            String permission = Build.VERSION.SDK_INT >= Build.VERSION_CODES.S
                    ? "BLUETOOTH_SCAN"
                    : "ACCESS_FINE_LOCATION";
            throw new SecurityException(
                    operation + ":fail permission denied (" + permission + ")");
        }
    }

    @SuppressLint("MissingPermission")
    private boolean isAdapterEnabled() {
        if (adapter == null || !hasConnectPermission()) {
            return false;
        }
        try {
            return adapter.isEnabled();
        } catch (SecurityException e) {
            return false;
        }
    }

    @SuppressLint("MissingPermission")
    private void stopScanner(BluetoothLeScanner scanner, ScanCallback callback) {
        if (scanner == null || callback == null) {
            return;
        }
        scanner.stopScan(callback);
    }

    @SuppressLint("MissingPermission")
    private void closeGatt(GattConnection connection, boolean disconnect) {
        if (connection == null) return;
        RuntimeException disconnectFailure = null;
        if (disconnect && hasConnectPermission()) {
            try {
                connection.disconnect();
            } catch (RuntimeException failure) {
                disconnectFailure = failure;
            }
        }
        try {
            connection.close();
        } catch (RuntimeException closeFailure) {
            if (disconnectFailure != null) closeFailure.addSuppressed(disconnectFailure);
            throw closeFailure;
        }
        // close() released the handle, so ownership must transfer even when disconnect
        // failed; reporting instead of throwing keeps a closed GATT from staying mapped.
        if (disconnectFailure != null) {
            reportGattCleanupFailure("BLE disconnect", disconnectFailure);
        }
    }

    private static BluetoothAdapter getAdapter(Context context) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            android.bluetooth.BluetoothManager bm = (android.bluetooth.BluetoothManager)
                    context.getSystemService(Context.BLUETOOTH_SERVICE);
            return bm != null ? bm.getAdapter() : null;
        }
        return BluetoothAdapter.getDefaultAdapter();
    }

    // ==================== Adapter ====================

    public void openAdapter(JSONObject opts, Reply reply) {
        if (adapter == null) {
            throw new BluetoothFailure(NOT_AVAILABLE, "not available");
        }
        requireConnectPermission("openBluetoothAdapter");
        // Claimed before any state is mutated, so a refusal leaves this manager
        // exactly as it was rather than half-opened.
        if (!adapterOpened && !ExclusiveDeviceArbiter.tryAcquire(
                ExclusiveDeviceArbiter.BLUETOOTH_ADAPTER, sessionId)) {
            throw new BluetoothFailure(SYSTEM_ERROR, "in use by another game");
        }
        adapterOpened = true;
        registerAdapterStateReceiver();
        // Opened either way, as the common platform has it: with Bluetooth off the
        // call fails, and the adapter-state event reports when it comes on.
        if (!isAdapterEnabled()) {
            throw new BluetoothFailure(NOT_AVAILABLE, "not available");
        }
        reply.ok();
    }

    public void closeAdapter(JSONObject opts, Reply reply) {
        closeAdapter();
        reply.ok();
    }

    public void closeAdapter() {
        adapterOpened = false;
        ResourceCleanup.runAll(
                () -> ExclusiveDeviceArbiter.release(
                        ExclusiveDeviceArbiter.BLUETOOTH_ADAPTER, sessionId),
                this::stopDiscoveryInternal,
                this::unregisterAdapterStateReceiver,
                discoveredDevices::clear,
                () -> ResourceCleanup.destroyMatching(
                        gattConnections, ignored -> true,
                        attempt -> {
                            attempt.failPending(NO_CONNECTION, "adapter closed");
                            closeGatt(attempt.beginClose(), true);
                        }),
                this::retryUnclosedCandidates,
                this::finishPairings,
                negotiatedMtu::clear,
                cachedRssi::clear);
    }

    public void getAdapterState(JSONObject opts, Reply reply) {
        requireOpened();
        JSONObject state = new JSONObject();
        try {
            state.put("discovering", discovering);
            state.put("available", isAdapterEnabled());
        } catch (JSONException ignored) {}
        reply.ok(state);
    }

    private void requireOpened() {
        if (adapter == null || !adapterOpened) {
            throw new BluetoothFailure(NOT_INIT, "not init");
        }
    }

    private static String requiredString(JSONObject opts, String name) {
        String value = opts.optString(name, "");
        if (value.isEmpty()) {
            throw new BluetoothFailure(INVALID_DATA, name + " is required");
        }
        return value;
    }

    // ==================== Device Discovery ====================

    public synchronized void startDiscovery(JSONObject opts, Reply reply) {
        requireOpened();
        String optionsJson = opts.toString();

        discoveredDevices.clear();
        LifecycleRequestState.Action action = discoveryRequest.requestStart(optionsJson);
        if (action == LifecycleRequestState.Action.NONE) {
            // Suspended with the session: the scan starts when it resumes.
            discovering = false;
            reply.ok();
            return;
        }
        if (action == LifecycleRequestState.Action.RESTART) {
            stopDiscoveryInternal();
        }

        try {
            startDiscoveryInternal(optionsJson);
            discovering = true;
            NativeMethods.onBluetoothAdapterStateChange(sessionId, isAdapterEnabled(), true);
        } catch (RuntimeException e) {
            stopDiscoveryInternal();
            discovering = false;
            discoveryRequest.startFailed(false);
            throw e;
        }
        reply.ok();
    }

    @SuppressLint("MissingPermission")
    private void startDiscoveryInternal(String optionsJson) {
        requireConnectPermission("startBluetoothDevicesDiscovery");
        requireScanPermission("startBluetoothDevicesDiscovery");
        if (adapter == null || !adapterOpened) {
            throw new BluetoothFailure(NOT_INIT, "not init");
        }
        if (!isAdapterEnabled()) {
            throw new BluetoothFailure(NOT_AVAILABLE, "not available");
        }

        List<ScanFilter> filters = new ArrayList<>();
        int scanMode = ScanSettings.SCAN_MODE_BALANCED;

        try {
            JSONObject opts = new JSONObject(optionsJson);
            JSONArray services = opts.optJSONArray("services");
            if (services != null) {
                for (int i = 0; i < services.length(); i++) {
                    String uuid = services.getString(i);
                    try {
                        ScanFilter filter = new ScanFilter.Builder()
                                .setServiceUuid(ParcelUuid.fromString(uuid))
                                .build();
                        filters.add(filter);
                    } catch (Exception e) {
                        Log.w(TAG, "Invalid service UUID: " + uuid);
                    }
                }
            }
            String powerLevel = opts.optString("powerLevel", "medium");
            switch (powerLevel) {
                case "low":
                    scanMode = ScanSettings.SCAN_MODE_LOW_POWER;
                    break;
                case "high":
                    scanMode = ScanSettings.SCAN_MODE_LOW_LATENCY;
                    break;
                default:
                    scanMode = ScanSettings.SCAN_MODE_BALANCED;
                    break;
            }
        } catch (JSONException ignored) {}

        leScanner = adapter.getBluetoothLeScanner();
        if (leScanner == null) {
            throw new BluetoothFailure(NOT_AVAILABLE, "scanner not available");
        }

        ScanSettings settings = new ScanSettings.Builder()
                .setScanMode(scanMode)
                .build();

        leScanCallback = new ScanCallback() {
            @Override
            public void onScanResult(int callbackType, ScanResult result) {
                synchronized (BluetoothManager.this) {
                    if (leScanCallback != this || !discoveryRequest.isActive()) return;
                    if (!hasConnectPermission() || !hasScanPermission()) {
                        stopDiscoveryInternal();
                        discovering = false;
                        discoveryRequest.startFailed(false);
                        NativeMethods.onBluetoothAdapterStateChange(sessionId, false, false);
                        return;
                    }
                    handleScanResult(result);
                }
            }

            @Override
            public void onBatchScanResults(List<ScanResult> results) {
                synchronized (BluetoothManager.this) {
                    if (leScanCallback != this || !discoveryRequest.isActive()) return;
                    if (!hasConnectPermission() || !hasScanPermission()) {
                        stopDiscoveryInternal();
                        discovering = false;
                        discoveryRequest.startFailed(false);
                        NativeMethods.onBluetoothAdapterStateChange(sessionId, false, false);
                        return;
                    }
                    for (ScanResult result : results) {
                        handleScanResult(result);
                    }
                }
            }

            @Override
            public void onScanFailed(int errorCode) {
                synchronized (BluetoothManager.this) {
                    if (leScanCallback != this || !discoveryRequest.isActive()) return;
                    Log.e(TAG, "BLE scan failed: " + errorCode);
                    leScanCallback = null;
                    discovering = false;
                    discoveryRequest.startFailed(false);
                    NativeMethods.onBluetoothAdapterStateChange(sessionId,
                            isAdapterEnabled(), false);
                }
            }
        };

        leScanner.startScan(filters.isEmpty() ? null : filters, settings, leScanCallback);
    }

    public synchronized void stopDiscovery(JSONObject opts, Reply reply) {
        stopDiscovery();
        reply.ok();
    }

    public synchronized void stopDiscovery() {
        boolean wasDiscovering = discovering;
        if (discoveryRequest.requestStop() == LifecycleRequestState.Action.STOP) {
            stopDiscoveryInternal();
            discovering = false;
        }
        if (wasDiscovering && adapter != null) {
            NativeMethods.onBluetoothAdapterStateChange(sessionId,
                    isAdapterEnabled(), false);
        }
    }

    private void stopDiscoveryInternal() {
        BluetoothLeScanner scanner = leScanner;
        ScanCallback callback = leScanCallback;
        stopScanner(scanner, callback);
        if (leScanCallback == callback) leScanCallback = null;
        if (leScanner == scanner) leScanner = null;
    }

    public void getDevices(JSONObject opts, Reply reply) {
        requireOpened();
        JSONArray devices = new JSONArray();
        for (JSONObject device : discoveredDevices.values()) {
            devices.put(device);
        }
        JSONObject result = new JSONObject();
        try {
            result.put("devices", devices);
        } catch (JSONException ignored) {}
        reply.ok(result);
    }

    /**
     * The devices this game is connected to that offer one of {@code services}.
     *
     * <p>A device is known to offer a service only once its services are
     * discovered, and Android tells an app the services of no connection but its
     * own -- so these are the game's own connections, which are also the only
     * ones it can use.
     */
    @SuppressLint("MissingPermission")
    public void getConnectedDevices(JSONObject opts, Reply reply) {
        requireOpened();
        requireConnectPermission("getConnectedBluetoothDevices");
        JSONArray wanted = opts.optJSONArray("services");
        JSONArray devices = new JSONArray();
        for (Map.Entry<String, GattAttempt> entry : gattConnections.entrySet()) {
            GattConnection connection = entry.getValue().connection();
            BluetoothGatt gatt = connection != null ? connection.raw() : null;
            if (gatt == null || !offersAny(gatt, wanted)) continue;
            JSONObject device = new JSONObject();
            try {
                String name = gatt.getDevice().getName();
                device.put("name", name != null ? name : "");
                device.put("deviceId", entry.getKey());
            } catch (JSONException | SecurityException ignored) {
                continue;
            }
            devices.put(device);
        }
        JSONObject result = new JSONObject();
        try {
            result.put("devices", devices);
        } catch (JSONException ignored) {}
        reply.ok(result);
    }

    private static boolean offersAny(BluetoothGatt gatt, JSONArray services) {
        if (services == null || services.length() == 0) return true;
        for (int i = 0; i < services.length(); i++) {
            try {
                if (gatt.getService(UUID.fromString(services.optString(i))) != null) return true;
            } catch (IllegalArgumentException notAUuid) {
                // Not a service this device could offer.
            }
        }
        return false;
    }

    // ==================== Pairing ====================

    /**
     * The main thread, for the timeouts of pairing and connecting. A holder rather
     * than a field: made on first use, by a request that has a timeout to keep.
     */
    private static final class MainThread {
        static final Handler HANDLER = new Handler(Looper.getMainLooper());
    }

    /** Pairings waiting on the system's answer, finished at teardown. */
    private final Set<PairingWatch> pairings = ConcurrentHashMap.newKeySet();

    /**
     * One {@code makeBluetoothPair}: the bond-state broadcasts for its device, the
     * PIN to give the pairing request, and the reply the outcome answers.
     */
    private final class PairingWatch extends BroadcastReceiver {
        private final Context context;
        private final String address;
        private final byte[] pin;
        private final Reply reply;
        private final AtomicBoolean finished = new AtomicBoolean(false);

        PairingWatch(Context context, String address, byte[] pin, Reply reply) {
            this.context = context;
            this.address = address;
            this.pin = pin;
            this.reply = reply;
        }

        void register() {
            IntentFilter filter = new IntentFilter(BluetoothDevice.ACTION_BOND_STATE_CHANGED);
            filter.addAction(BluetoothDevice.ACTION_PAIRING_REQUEST);
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                // System broadcasts reach a non-exported receiver; nothing else should.
                context.registerReceiver(this, filter, Context.RECEIVER_NOT_EXPORTED);
            } else {
                context.registerReceiver(this, filter);
            }
            pairings.add(this);
        }

        /** Stop listening; true for the one caller that gets to answer. */
        boolean finish() {
            if (!finished.compareAndSet(false, true)) return false;
            pairings.remove(this);
            try {
                context.unregisterReceiver(this);
            } catch (IllegalArgumentException alreadyGone) {
                // Unregistered with its context.
            }
            return true;
        }

        @SuppressLint("MissingPermission")
        @Override
        public void onReceive(Context ctx, Intent intent) {
            BluetoothDevice device = intent.getParcelableExtra(BluetoothDevice.EXTRA_DEVICE);
            if (device == null || !address.equalsIgnoreCase(device.getAddress())) return;
            if (BluetoothDevice.ACTION_PAIRING_REQUEST.equals(intent.getAction())) {
                if (pin.length > 0 && hasConnectPermission()) device.setPin(pin);
                return;
            }
            int state = intent.getIntExtra(BluetoothDevice.EXTRA_BOND_STATE, BluetoothDevice.ERROR);
            if (state == BluetoothDevice.BOND_BONDED) {
                if (finish()) reply.ok();
            } else if (state == BluetoothDevice.BOND_NONE) {
                if (finish()) reply.fail(CONNECTION_FAIL, "pairing failed");
            }
        }
    }

    private void finishPairings() {
        for (PairingWatch watch : new ArrayList<>(pairings)) {
            if (watch.finish()) watch.reply.fail(NOT_INIT, "adapter closed");
        }
    }

    private BluetoothDevice remoteDevice(String deviceId) {
        try {
            return adapter.getRemoteDevice(deviceId);
        } catch (IllegalArgumentException notAnAddress) {
            throw new BluetoothFailure(NO_DEVICE, "no device " + deviceId);
        }
    }

    @SuppressLint("MissingPermission")
    public void makePair(JSONObject opts, Reply reply) {
        if (adapter == null) {
            throw new BluetoothFailure(NOT_AVAILABLE, "not available");
        }
        requireConnectPermission("makeBluetoothPair");
        String deviceId = requiredString(opts, "deviceId");
        BluetoothDevice device = remoteDevice(deviceId);
        if (device.getBondState() == BluetoothDevice.BOND_BONDED) {
            reply.ok();
            return;
        }
        Activity activity = getActivity();
        if (activity == null) {
            throw new BluetoothFailure(SYSTEM_ERROR, "no activity");
        }
        byte[] pin = hexToBytes(opts.optString("pin", ""));
        PairingWatch watch = new PairingWatch(activity, device.getAddress(), pin, reply);
        watch.register();
        if (!device.createBond()) {
            watch.finish();
            throw new BluetoothFailure(CONNECTION_FAIL, "pairing could not start");
        }
        int timeout = opts.optInt("timeout", 20000);
        MainThread.HANDLER.postDelayed(() -> {
            if (watch.finish()) reply.fail(OPERATE_TIME_OUT, "timeout");
        }, timeout > 0 ? timeout : 20000);
    }

    @SuppressLint("MissingPermission")
    public void isDevicePaired(JSONObject opts, Reply reply) {
        if (adapter == null) {
            throw new BluetoothFailure(NOT_AVAILABLE, "not available");
        }
        requireConnectPermission("isBluetoothDevicePaired");
        String deviceId = requiredString(opts, "deviceId");
        if (remoteDevice(deviceId).getBondState() == BluetoothDevice.BOND_BONDED) {
            reply.ok();
        } else {
            reply.fail(NO_DEVICE, "not paired");
        }
    }

    // ==================== Beacon ====================

    // Beacon discovery reuses BLE scanning with iBeacon/Eddystone parsing.
    // For simplicity, we use a separate flag and parse manufacturer data.

    private volatile boolean beaconDiscovering = false;
    private BluetoothLeScanner beaconScanner;
    private ScanCallback beaconScanCallback;
    private final ConcurrentHashMap<String, JSONObject> discoveredBeacons = new ConcurrentHashMap<>();

    public synchronized void startBeaconDiscovery(JSONObject opts, Reply reply) {
        String optionsJson = opts.toString();
        requireConnectPermission("startBeaconDiscovery");
        requireScanPermission("startBeaconDiscovery");
        if (adapter == null || !isAdapterEnabled()) {
            throw new BluetoothFailure(BEACON_UNAVAILABLE, "bluetooth unavailable");
        }

        discoveredBeacons.clear();
        LifecycleRequestState.Action action = beaconRequest.requestStart(optionsJson);
        if (action == LifecycleRequestState.Action.NONE) {
            beaconDiscovering = false;
            reply.ok();
            return;
        }
        if (action == LifecycleRequestState.Action.RESTART) {
            stopBeaconDiscoveryInternal();
        }

        try {
            startBeaconDiscoveryInternal();
            beaconDiscovering = true;
            NativeMethods.onBeaconServiceChange(sessionId, true, true);
        } catch (RuntimeException e) {
            stopBeaconDiscoveryInternal();
            beaconDiscovering = false;
            beaconRequest.startFailed(false);
            throw e;
        }
        reply.ok();
    }

    @SuppressLint("MissingPermission")
    private void startBeaconDiscoveryInternal() {
        requireConnectPermission("startBeaconDiscovery");
        requireScanPermission("startBeaconDiscovery");
        if (adapter == null || !isAdapterEnabled()) {
            throw new BluetoothFailure(BEACON_UNAVAILABLE, "bluetooth unavailable");
        }

        beaconScanner = adapter.getBluetoothLeScanner();
        if (beaconScanner == null) {
            throw new BluetoothFailure(BEACON_UNAVAILABLE, "scanner not available");
        }

        ScanSettings settings = new ScanSettings.Builder()
                .setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY)
                .build();

        beaconScanCallback = new ScanCallback() {
            @Override
            public void onScanResult(int callbackType, ScanResult result) {
                synchronized (BluetoothManager.this) {
                    if (beaconScanCallback != this || !beaconRequest.isActive()) return;
                    if (!hasConnectPermission() || !hasScanPermission()) {
                        stopBeaconDiscoveryInternal();
                        beaconDiscovering = false;
                        beaconRequest.startFailed(false);
                        NativeMethods.onBeaconServiceChange(sessionId, false, false);
                        return;
                    }
                    handleBeaconResult(result);
                }
            }

            @Override
            public void onBatchScanResults(List<ScanResult> results) {
                synchronized (BluetoothManager.this) {
                    if (beaconScanCallback != this || !beaconRequest.isActive()) return;
                    if (!hasConnectPermission() || !hasScanPermission()) {
                        stopBeaconDiscoveryInternal();
                        beaconDiscovering = false;
                        beaconRequest.startFailed(false);
                        NativeMethods.onBeaconServiceChange(sessionId, false, false);
                        return;
                    }
                    for (ScanResult r : results) {
                        handleBeaconResult(r);
                    }
                }
            }

            @Override
            public void onScanFailed(int errorCode) {
                synchronized (BluetoothManager.this) {
                    if (beaconScanCallback != this || !beaconRequest.isActive()) return;
                    Log.e(TAG, "Beacon scan failed: " + errorCode);
                    beaconScanCallback = null;
                    beaconDiscovering = false;
                    beaconRequest.startFailed(false);
                    NativeMethods.onBeaconServiceChange(sessionId, false, false);
                }
            }
        };

        beaconScanner.startScan(null, settings, beaconScanCallback);
    }

    public synchronized void stopBeaconDiscovery(JSONObject opts, Reply reply) {
        stopBeaconDiscovery();
        reply.ok();
    }

    public synchronized void stopBeaconDiscovery() {
        boolean wasDiscovering = beaconDiscovering;
        if (beaconRequest.requestStop() == LifecycleRequestState.Action.STOP) {
            stopBeaconDiscoveryInternal();
            beaconDiscovering = false;
        }
        if (wasDiscovering) {
            NativeMethods.onBeaconServiceChange(sessionId,
                    isAdapterEnabled(), false);
        }
    }

    private void stopBeaconDiscoveryInternal() {
        BluetoothLeScanner scanner = beaconScanner;
        ScanCallback callback = beaconScanCallback;
        stopScanner(scanner, callback);
        if (beaconScanCallback == callback) beaconScanCallback = null;
        if (beaconScanner == scanner) beaconScanner = null;
    }

    public void getBeacons(JSONObject opts, Reply reply) {
        JSONArray beacons = new JSONArray();
        for (JSONObject beacon : discoveredBeacons.values()) {
            beacons.put(beacon);
        }
        JSONObject result = new JSONObject();
        try {
            result.put("beacons", beacons);
        } catch (JSONException ignored) {}
        reply.ok(result);
    }

    // ==================== BLE GATT ====================

    @SuppressLint("MissingPermission")
    /**
     * Connect, answering once the device's services are discovered -- so the
     * getBLEDeviceServices a game makes next finds them -- or failing on a
     * disconnect, a failed discovery, or {@code timeout} milliseconds.
     */
    public void createBLEConnection(JSONObject opts, Reply reply) {
        if (adapter == null) {
            throw new BluetoothFailure(NOT_AVAILABLE, "not available");
        }
        requireConnectPermission("createBLEConnection");
        {
            String deviceId = requiredString(opts, "deviceId");

            GattAttempt attempt = new GattAttempt();
            if (gattConnections.putIfAbsent(deviceId, attempt) != null) {
                throw new BluetoothFailure(ALREADY_CONNECTED, "already connect");
            }
            attempt.awaitConnection(reply);

            try {
                BluetoothDevice device = remoteDevice(deviceId);
                Activity activity = getActivity();
                Context ctx = activity != null ? activity : null;
                if (ctx == null) {
                    throw new BluetoothFailure(SYSTEM_ERROR, "no activity");
                }

                BluetoothGattCallback callback = new BluetoothGattCallback() {
                    /**
                     * The wrapper for this connection, kept rather than rebuilt.
                     *
                     * <p>The platform hands the same {@code BluetoothGatt} to
                     * every callback of one connection, so wrapping it per
                     * callback allocated an object per notification to express
                     * a value that never changes. Volatile with an identity
                     * check rather than a lock: the check is what makes it
                     * correct if the platform ever hands over a different
                     * handle, and a lost race costs one wrapper.
                     */
                    private volatile AndroidGattConnection wrapper;

                    private GattConnection connectionFor(BluetoothGatt gatt) {
                        AndroidGattConnection current = wrapper;
                        if (current == null || current.raw() != gatt) {
                            current = new AndroidGattConnection(gatt);
                            wrapper = current;
                        }
                        return current;
                    }

                    @Override
                    public void onConnectionStateChange(
                            BluetoothGatt gatt, int status, int newState) {
                        boolean connected = newState == BluetoothProfile.STATE_CONNECTED
                                && hasConnectPermission();
                        handleGattConnectionStateChange(
                                deviceId, attempt, connectionFor(gatt), connected);
                    }

                    @Override
                    public void onServicesDiscovered(BluetoothGatt gatt, int status) {
                        // The services are cached in the BluetoothGatt object; the
                        // connection the game asked for is ready now.
                        handleGattServicesDiscovered(
                                deviceId, attempt, status == BluetoothGatt.GATT_SUCCESS);
                    }

                    @Override
                    public void onCharacteristicWrite(
                            BluetoothGatt gatt,
                            BluetoothGattCharacteristic characteristic,
                            int status) {
                        answerPending(attempt.pendingWrites,
                                characteristicKey(characteristic), status, "write failed");
                    }

                    @Override
                    public void onDescriptorWrite(
                            BluetoothGatt gatt,
                            BluetoothGattDescriptor descriptor,
                            int status) {
                        answerPending(attempt.pendingDescriptorWrites,
                                characteristicKey(descriptor.getCharacteristic()), status,
                                "notification setup failed");
                    }

                    @Override
                    public void onCharacteristicRead(
                            BluetoothGatt gatt,
                            BluetoothGattCharacteristic characteristic,
                            int status) {
                        if (status != BluetoothGatt.GATT_SUCCESS) return;
                        byte[] value = characteristic.getValue();
                        if (value == null) value = NO_VALUE;
                        handleGattCharacteristicRead(
                                deviceId,
                                attempt,
                                connectionFor(gatt),
                                uuidText(characteristic.getService().getUuid()),
                                uuidText(characteristic.getUuid()),
                                value);
                    }

                    @Override
                    public void onCharacteristicChanged(
                            BluetoothGatt gatt,
                            BluetoothGattCharacteristic characteristic) {
                        byte[] value = characteristic.getValue();
                        if (value == null) value = NO_VALUE;
                        handleGattCharacteristicChanged(
                                deviceId,
                                attempt,
                                connectionFor(gatt),
                                uuidText(characteristic.getService().getUuid()),
                                uuidText(characteristic.getUuid()),
                                value);
                    }

                    @Override
                    public void onMtuChanged(BluetoothGatt gatt, int mtu, int status) {
                        if (status == BluetoothGatt.GATT_SUCCESS) {
                            handleGattMtuChanged(
                                    deviceId,
                                    attempt,
                                    new AndroidGattConnection(gatt),
                                    mtu);
                        }
                        Reply pending;
                        synchronized (attempt) {
                            pending = attempt.pendingMtu;
                            attempt.pendingMtu = null;
                        }
                        if (pending == null) return;
                        if (status != BluetoothGatt.GATT_SUCCESS) {
                            pending.fail(SYSTEM_ERROR, "MTU negotiation failed (status " + status + ")");
                            return;
                        }
                        JSONObject result = new JSONObject();
                        try {
                            result.put("mtu", mtu);
                        } catch (JSONException ignored) {}
                        pending.ok(result);
                    }

                    @Override
                    public void onReadRemoteRssi(BluetoothGatt gatt, int rssi, int status) {
                        if (status == BluetoothGatt.GATT_SUCCESS) {
                            handleGattRssiChanged(
                                    deviceId,
                                    attempt,
                                    new AndroidGattConnection(gatt),
                                    rssi);
                        }
                        List<Reply> waiting;
                        synchronized (attempt) {
                            waiting = new ArrayList<>(attempt.pendingRssi);
                            attempt.pendingRssi.clear();
                        }
                        for (Reply pending : waiting) {
                            if (status != BluetoothGatt.GATT_SUCCESS) {
                                pending.fail(SYSTEM_ERROR, "RSSI read failed (status " + status + ")");
                                continue;
                            }
                            JSONObject result = new JSONObject();
                            try {
                                result.put("RSSI", rssi);
                            } catch (JSONException ignored) {}
                            pending.ok(result);
                        }
                    }
                };

                BluetoothGatt gatt = device.connectGatt(
                        ctx, false, callback, BluetoothDevice.TRANSPORT_LE);
                if (gatt == null) {
                    throw new BluetoothFailure(CONNECTION_FAIL, "connection failed");
                }
                if (!publishGattConnection(
                        deviceId, attempt, new AndroidGattConnection(gatt))) {
                    throw new BluetoothFailure(CONNECTION_FAIL, "connection cancelled");
                }
            } catch (RuntimeException failure) {
                attempt.takeConnectReply();
                if (attempt.connection() == null) abandonGattAttempt(deviceId, attempt);
                throw failure;
            }
            int timeout = opts.optInt("timeout", 0);
            if (timeout > 0) {
                MainThread.HANDLER.postDelayed(() -> {
                    Reply pending = attempt.takeConnectReply();
                    if (pending == null) return;
                    try {
                        closeAndRemoveGatt(deviceId, attempt, true);
                    } catch (RuntimeException cleanupFailure) {
                        reportGattCleanupFailure("BLE connect timeout cleanup", cleanupFailure);
                    }
                    pending.fail(OPERATE_TIME_OUT, "timeout");
                }, timeout);
            }
        }
    }

    public void closeBLEConnection(JSONObject opts, Reply reply) {
        closeGattConnection(requiredString(opts, "deviceId"));
        reply.ok();
    }

    /** The connection a request needs, or the failure for not having it. */
    private BluetoothGatt connectedGatt(String deviceId) {
        BluetoothGatt gatt = rawGatt(deviceId);
        if (gatt == null) {
            throw new BluetoothFailure(NO_CONNECTION, "no connection");
        }
        return gatt;
    }

    /** {@code service/characteristic}: what a pending GATT request is keyed by. */
    private String characteristicKey(BluetoothGattCharacteristic characteristic) {
        return uuidText(characteristic.getService().getUuid()) + "/"
                + uuidText(characteristic.getUuid());
    }

    private static void answerPending(
            ConcurrentHashMap<String, Reply> pending, String key, int status, String failure) {
        Reply reply = pending.remove(key);
        if (reply == null) return;
        if (status == BluetoothGatt.GATT_SUCCESS) {
            reply.ok();
        } else {
            reply.fail(SYSTEM_ERROR, failure + " (status " + status + ")");
        }
    }

    void handleGattServicesDiscovered(String deviceId, GattAttempt attempt, boolean discovered) {
        Reply pending = attempt.takeConnectReply();
        if (pending == null) return;
        if (discovered && gattConnections.get(deviceId) == attempt) {
            pending.ok();
        } else {
            pending.fail(CONNECTION_FAIL, "service discovery failed");
        }
    }

    void closeGattConnection(String deviceId) {
        // Before closing this device, finish what is already owed. Running first means
        // a retry failure is reported rather than masking the caller's own close.
        retryUnclosedCandidates();
        GattAttempt attempt = gattConnections.get(deviceId);
        closeAndRemoveGatt(deviceId, attempt, true);
    }

    /**
     * Retries every retained candidate close, keeping the ones that fail again.
     *
     * <p>Snapshotted before iterating because a concurrent {@code publishGattConnection}
     * may add to the set, and a retry is never required to also handle arrivals that
     * happen while it runs -- the next close will.
     */
    private void retryUnclosedCandidates() {
        for (GattConnection candidate : new ArrayList<>(unclosedCandidates)) {
            try {
                closeGatt(candidate, false);
                unclosedCandidates.remove(candidate);
            } catch (RuntimeException retryFailure) {
                reportGattCleanupFailure("BLE candidate close retry", retryFailure);
            }
        }
    }

    int unclosedCandidateCountForTests() {
        return unclosedCandidates.size();
    }

    void handleGattConnectionStateChange(
            String deviceId,
            GattAttempt attempt,
            GattConnection connection,
            boolean connected) {
        if (!publishGattConnection(deviceId, attempt, connection)) {
            reportRetiredAttemptDisconnected(deviceId, attempt);
            return;
        }
        if (!connected) {
            Reply connecting = attempt.takeConnectReply();
            if (connecting != null) connecting.fail(CONNECTION_FAIL, "connection failed");
            try {
                closeAndRemoveGatt(deviceId, attempt, false);
            } catch (RuntimeException cleanupFailure) {
                reportGattCleanupFailure("BLE disconnect cleanup", cleanupFailure);
            }
            reportRetiredAttemptDisconnected(deviceId, attempt);
            return;
        }
        boolean admitted = gattCallbackAdmission.run(() ->
                gattConnections.get(deviceId) == attempt
                        && attempt.dispatchIfActive(
                                connection,
                                gattConnectPermissionGranted,
                                gattSessionTerminated,
                                () -> discoverGattServicesAndReport(deviceId, attempt, connection)));
        if (!admitted) reportRetiredAttemptDisconnected(deviceId, attempt);
    }

    /**
     * Reports a failed or retired attempt as disconnected. A live replacement owns the
     * device's observable state, so a superseded attempt must never overwrite it.
     */
    private void reportRetiredAttemptDisconnected(String deviceId, GattAttempt attempt) {
        reportConnectionState(deviceId, attempt, false);
    }

    /**
     * Deliver one connection-state report, if the attempt making it still speaks
     * for the device at the moment of delivery.
     *
     * <p>The re-check and the delivery are one step under
     * {@link #connectionStateOrder}, which is the whole of the fix: the same
     * check outside a monitor is a decision that can go stale between being made
     * and being acted on.
     *
     * <p><b>The two directions are not symmetric, and the asymmetry is the
     * semantics.</b> A <em>disconnect</em> from an attempt the map no longer
     * holds is precisely the report that must arrive -- retirement is what
     * removed the entry. A <em>connect</em> from an attempt the map no longer
     * holds must not: no owner means nothing is entitled to claim the device is
     * connected. Reporting `connected` unconditionally is how a superseded
     * attempt's late service-discovery result used to overwrite a completed
     * teardown.
     */
    private void reportConnectionState(String deviceId, GattAttempt attempt, boolean connected) {
        synchronized (connectionStateOrder) {
            GattAttempt current = gattConnections.get(deviceId);
            if (connected ? current != attempt : current != null && current != attempt) {
                return;
            }
            connectionStateReporter.report(deviceId, connected);
        }
    }

    private void discoverGattServicesAndReport(
            String deviceId,
            GattAttempt attempt,
            GattConnection connection) {
        boolean discovered;
        try {
            discovered = connection.discoverServices();
        } catch (RuntimeException discoverFailure) {
            discovered = false;
            Log.w(TAG, "discoverServices failed for " + deviceId, discoverFailure);
        }
        if (!discovered) {
            Reply connecting = attempt.takeConnectReply();
            if (connecting != null) connecting.fail(CONNECTION_FAIL, "service discovery failed");
            try {
                closeAndRemoveGatt(deviceId, attempt, true);
            } catch (RuntimeException cleanupFailure) {
                reportGattCleanupFailure(
                        "BLE service discovery cleanup", cleanupFailure);
            }
        }
        reportConnectionState(deviceId, attempt, discovered);
    }

    /**
     * The canonical text of {@code uuid}, formatted once per session.
     *
     * <p>Explicit lookup-then-insert rather than {@code computeIfAbsent}: the
     * hit path is the one a notification takes, and it must reach the heap
     * neither for a mapping function nor for anything else. A lost race stores
     * an equal string twice, which costs nothing but the loser.
     */
    String uuidText(UUID uuid) {
        String text = uuidText.get(uuid);
        if (text != null) return text;
        text = uuid.toString();
        if (uuidText.size() < UUID_TEXT_CACHE_LIMIT) {
            uuidText.put(uuid, text);
        }
        return text;
    }

    int uuidTextCacheSizeForTests() {
        return uuidText.size();
    }

    /**
     * The report-ordering monitor, so a test can hold it and choose the
     * interleaving instead of hoping for one.
     *
     * <p>Exposed for the same reason the Rust contention probe is handed a
     * registry's lock: the property under test is that a decision and its
     * delivery are one step, and the only way to demonstrate that is to stop a
     * thread between them -- which requires holding what it will block on.
     */
    Object connectionStateOrderForTests() {
        return connectionStateOrder;
    }

    boolean hasGattConnection(String deviceId, GattConnection connection) {
        GattAttempt attempt = gattConnections.get(deviceId);
        return attempt != null && attempt.connection() == connection;
    }

    boolean handleGattCharacteristicRead(
            String deviceId,
            GattAttempt attempt,
            GattConnection connection,
            String serviceId,
            String characteristicId,
            byte[] value) {
        return dispatchCharacteristic(
                deviceId, attempt, connection, serviceId, characteristicId, value);
    }

    boolean handleGattCharacteristicChanged(
            String deviceId,
            GattAttempt attempt,
            GattConnection connection,
            String serviceId,
            String characteristicId,
            byte[] value) {
        return dispatchCharacteristic(
                deviceId, attempt, connection, serviceId, characteristicId, value);
    }

    /**
     * Admit and deliver one characteristic value without allocating.
     *
     * <p>The same admission and liveness sequence {@link #dispatchGattCallback}
     * performs, reached through the attempt's reusable carrier rather than
     * through two lambdas built for this event. A read takes it too: it is the
     * same delivery, and giving the cold path its own closure-shaped copy would
     * leave two spellings of one sequence to drift apart.
     */
    private boolean dispatchCharacteristic(
            String deviceId,
            GattAttempt attempt,
            GattConnection connection,
            String serviceId,
            String characteristicId,
            byte[] value) {
        if (attempt == null) return false;
        CharacteristicDispatch dispatch = attempt.dispatch(this);
        synchronized (dispatch) {
            dispatch.fill(deviceId, connection, serviceId, characteristicId, value);
            try {
                return gattCallbackAdmission.run(dispatch);
            } finally {
                dispatch.clear();
            }
        }
    }

    boolean handleGattMtuChanged(
            String deviceId,
            GattAttempt attempt,
            GattConnection connection,
            int mtu) {
        return dispatchGattCallback(deviceId, attempt, connection, () -> {
            negotiatedMtu.put(deviceId, mtu);
            gattEventReporter.mtu(deviceId, mtu);
        });
    }

    boolean handleGattRssiChanged(
            String deviceId,
            GattAttempt attempt,
            GattConnection connection,
            int rssi) {
        return dispatchGattCallback(
                deviceId,
                attempt,
                connection,
                () -> cachedRssi.put(deviceId, rssi));
    }

    Integer cachedMtuForTests(String deviceId) {
        return negotiatedMtu.get(deviceId);
    }

    Integer cachedRssiForTests(String deviceId) {
        return cachedRssi.get(deviceId);
    }

    private boolean dispatchGattCallback(
            String deviceId,
            GattAttempt attempt,
            GattConnection connection,
            Runnable callback) {
        if (attempt == null) return false;
        return gattCallbackAdmission.run(() ->
                gattConnections.get(deviceId) == attempt
                        && attempt.dispatchIfActive(
                                connection,
                                gattConnectPermissionGranted,
                                gattSessionTerminated,
                                callback));
    }

    private BluetoothGatt rawGatt(String deviceId) {
        GattAttempt attempt = gattConnections.get(deviceId);
        GattConnection connection = attempt != null ? attempt.connection() : null;
        return connection != null ? connection.raw() : null;
    }

    private void closeAndRemoveGatt(
            String deviceId,
            GattAttempt attempt,
            boolean disconnect) {
        if (attempt == null) return;
        attempt.failPending(NO_CONNECTION, "no connection");
        GattConnection connection = attempt.beginClose();
        closeGatt(connection, disconnect);
        if (gattConnections.remove(deviceId, attempt)) {
            negotiatedMtu.remove(deviceId);
            cachedRssi.remove(deviceId);
        }
    }

    GattAttempt beginGattAttempt(String deviceId) {
        GattAttempt attempt = new GattAttempt();
        return gattConnections.putIfAbsent(deviceId, attempt) == null ? attempt : null;
    }

    void abandonGattAttempt(String deviceId, GattAttempt attempt) {
        if (attempt == null) return;
        attempt.beginClose();
        gattConnections.remove(deviceId, attempt);
    }

    boolean publishGattConnection(
            String deviceId,
            GattAttempt attempt,
            GattConnection connection) {
        if (attempt != null
                && gattConnections.get(deviceId) == attempt
                && attempt.attach(connection)) {
            return true;
        }
        try {
            closeGatt(connection, false);
        } catch (RuntimeException cleanupFailure) {
            unclosedCandidates.add(connection);
            reportGattCleanupFailure("BLE late callback cleanup", cleanupFailure);
        }
        return false;
    }

    private void reportGattCleanupFailure(String operation, RuntimeException failure) {
        try {
            cleanupFailureReporter.report(operation, failure);
        } catch (RuntimeException reportFailure) {
            failure.addSuppressed(reportFailure);
            Log.e(TAG, "GATT cleanup failure reporting failed", failure);
        }
    }

    @SuppressLint("MissingPermission")
    public void getBLEDeviceServices(JSONObject opts, Reply reply) {
        requireConnectPermission("getBLEDeviceServices");
        BluetoothGatt gatt = connectedGatt(requiredString(opts, "deviceId"));
        JSONArray services = new JSONArray();
        try {
            for (BluetoothGattService service : gatt.getServices()) {
                JSONObject entry = new JSONObject();
                entry.put("uuid", uuidText(service.getUuid()));
                entry.put("isPrimary", service.getType() == BluetoothGattService.SERVICE_TYPE_PRIMARY);
                services.put(entry);
            }
            reply.ok(new JSONObject().put("services", services));
        } catch (JSONException impossible) {
            throw new BluetoothFailure(SYSTEM_ERROR, "result serialisation failed");
        }
    }

    @SuppressLint("MissingPermission")
    public void getBLEDeviceCharacteristics(JSONObject opts, Reply reply) {
        requireConnectPermission("getBLEDeviceCharacteristics");
        BluetoothGatt gatt = connectedGatt(requiredString(opts, "deviceId"));
        BluetoothGattService service = findService(gatt, requiredString(opts, "serviceId"));
        JSONArray characteristics = new JSONArray();
        try {
            for (BluetoothGattCharacteristic ch : service.getCharacteristics()) {
                int p = ch.getProperties();
                JSONObject props = new JSONObject();
                props.put("read", (p & BluetoothGattCharacteristic.PROPERTY_READ) != 0);
                props.put("write", (p & BluetoothGattCharacteristic.PROPERTY_WRITE) != 0);
                props.put("notify", (p & BluetoothGattCharacteristic.PROPERTY_NOTIFY) != 0);
                props.put("indicate", (p & BluetoothGattCharacteristic.PROPERTY_INDICATE) != 0);
                props.put("writeNoResponse",
                        (p & BluetoothGattCharacteristic.PROPERTY_WRITE_NO_RESPONSE) != 0);
                props.put("writeDefault", (p & BluetoothGattCharacteristic.PROPERTY_WRITE) != 0);
                JSONObject entry = new JSONObject();
                entry.put("uuid", uuidText(ch.getUuid()));
                entry.put("properties", props);
                characteristics.put(entry);
            }
            reply.ok(new JSONObject().put("characteristics", characteristics));
        } catch (JSONException impossible) {
            throw new BluetoothFailure(SYSTEM_ERROR, "result serialisation failed");
        }
    }

    /** Answered once the read is issued: the value arrives as the characteristic event. */
    @SuppressLint("MissingPermission")
    public void readBLECharacteristicValue(JSONObject opts, Reply reply) {
        requireConnectPermission("readBLECharacteristicValue");
        BluetoothGatt gatt = connectedGatt(requiredString(opts, "deviceId"));
        BluetoothGattCharacteristic ch = findCharacteristic(gatt,
                requiredString(opts, "serviceId"), requiredString(opts, "characteristicId"));
        if ((ch.getProperties() & BluetoothGattCharacteristic.PROPERTY_READ) == 0) {
            throw new BluetoothFailure(PROPERTY_NOT_SUPPORT, "property not support");
        }
        if (!gatt.readCharacteristic(ch)) {
            throw new BluetoothFailure(SYSTEM_ERROR, "read request failed");
        }
        reply.ok();
    }

    /**
     * Answered when the peripheral acknowledges the write -- or, for
     * {@code writeNoResponse}, which has no acknowledgement, once it is sent.
     */
    @SuppressLint("MissingPermission")
    @SuppressWarnings("deprecation")
    public void writeBLECharacteristicValue(JSONObject opts, Reply reply) {
        requireConnectPermission("writeBLECharacteristicValue");
        String deviceId = requiredString(opts, "deviceId");
        BluetoothGatt gatt = connectedGatt(deviceId);
        BluetoothGattCharacteristic ch = findCharacteristic(gatt,
                requiredString(opts, "serviceId"), requiredString(opts, "characteristicId"));
        byte[] value = hexToBytes(opts.optString("value", ""));
        boolean noResponse = "writeNoResponse".equals(opts.optString("writeType", "write"));
        int required = noResponse
                ? BluetoothGattCharacteristic.PROPERTY_WRITE_NO_RESPONSE
                : BluetoothGattCharacteristic.PROPERTY_WRITE;
        if ((ch.getProperties() & required) == 0) {
            throw new BluetoothFailure(PROPERTY_NOT_SUPPORT, "property not support");
        }
        GattAttempt attempt = gattConnections.get(deviceId);
        String key = characteristicKey(ch);
        if (!noResponse && attempt != null && attempt.pendingWrites.putIfAbsent(key, reply) != null) {
            throw new BluetoothFailure(SYSTEM_ERROR, "a write to this characteristic is in progress");
        }
        int writeType = noResponse
                ? BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE
                : BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT;
        boolean sent;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            sent = gatt.writeCharacteristic(ch, value, writeType) == BluetoothGatt.GATT_SUCCESS;
        } else {
            ch.setValue(value);
            ch.setWriteType(writeType);
            sent = gatt.writeCharacteristic(ch);
        }
        if (!sent) {
            if (attempt != null) attempt.pendingWrites.remove(key, reply);
            throw new BluetoothFailure(SYSTEM_ERROR, "write request failed");
        }
        if (noResponse || attempt == null) reply.ok();
    }

    /** Answered when the peripheral has taken the subscription (its CCCD written). */
    @SuppressLint("MissingPermission")
    @SuppressWarnings("deprecation")
    public void notifyBLECharacteristicValueChange(JSONObject opts, Reply reply) {
        requireConnectPermission("notifyBLECharacteristicValueChange");
        String deviceId = requiredString(opts, "deviceId");
        BluetoothGatt gatt = connectedGatt(deviceId);
        BluetoothGattCharacteristic ch = findCharacteristic(gatt,
                requiredString(opts, "serviceId"), requiredString(opts, "characteristicId"));
        boolean state = opts.optBoolean("state", true);
        int props = ch.getProperties();
        boolean indicate = (props & BluetoothGattCharacteristic.PROPERTY_INDICATE) != 0;
        boolean notify = (props & BluetoothGattCharacteristic.PROPERTY_NOTIFY) != 0;
        if (state && !indicate && !notify) {
            throw new BluetoothFailure(PROPERTY_NOT_SUPPORT, "property not support");
        }
        if (!gatt.setCharacteristicNotification(ch, state)) {
            throw new BluetoothFailure(SYSTEM_ERROR, "set notification failed");
        }
        BluetoothGattDescriptor cccd = ch.getDescriptor(CCCD_UUID);
        if (cccd == null) {
            // Nothing to write on the peripheral's side: local delivery is all there is.
            reply.ok();
            return;
        }
        byte[] descriptorValue = !state
                ? BluetoothGattDescriptor.DISABLE_NOTIFICATION_VALUE
                : indicate && (!notify || "indication".equals(opts.optString("type", "indication")))
                        ? BluetoothGattDescriptor.ENABLE_INDICATION_VALUE
                        : BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE;
        GattAttempt attempt = gattConnections.get(deviceId);
        String key = characteristicKey(ch);
        if (attempt != null && attempt.pendingDescriptorWrites.putIfAbsent(key, reply) != null) {
            throw new BluetoothFailure(SYSTEM_ERROR, "a subscription change is in progress");
        }
        boolean sent;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            sent = gatt.writeDescriptor(cccd, descriptorValue) == BluetoothGatt.GATT_SUCCESS;
        } else {
            cccd.setValue(descriptorValue);
            sent = gatt.writeDescriptor(cccd);
        }
        if (!sent) {
            if (attempt != null) attempt.pendingDescriptorWrites.remove(key, reply);
            throw new BluetoothFailure(SYSTEM_ERROR, "descriptor write failed");
        }
        if (attempt == null) reply.ok();
    }

    /** Answered with the RSSI read from the device for this request. */
    @SuppressLint("MissingPermission")
    public void getBLEDeviceRSSI(JSONObject opts, Reply reply) {
        requireConnectPermission("getBLEDeviceRSSI");
        String deviceId = requiredString(opts, "deviceId");
        BluetoothGatt gatt = connectedGatt(deviceId);
        GattAttempt attempt = gattConnections.get(deviceId);
        if (attempt == null) {
            throw new BluetoothFailure(NO_CONNECTION, "no connection");
        }
        boolean first;
        synchronized (attempt) {
            first = attempt.pendingRssi.isEmpty();
            attempt.pendingRssi.add(reply);
        }
        // Requests that arrive while a read is in flight share its answer.
        if (first && !gatt.readRemoteRssi()) {
            synchronized (attempt) {
                attempt.pendingRssi.remove(reply);
            }
            throw new BluetoothFailure(SYSTEM_ERROR, "RSSI read failed");
        }
    }

    /** Answered with the MTU the peripheral agreed to. */
    @SuppressLint("MissingPermission")
    public void setBLEMTU(JSONObject opts, Reply reply) {
        requireConnectPermission("setBLEMTU");
        String deviceId = requiredString(opts, "deviceId");
        BluetoothGatt gatt = connectedGatt(deviceId);
        GattAttempt attempt = gattConnections.get(deviceId);
        if (attempt == null) {
            throw new BluetoothFailure(NO_CONNECTION, "no connection");
        }
        synchronized (attempt) {
            if (attempt.pendingMtu != null) {
                throw new BluetoothFailure(SYSTEM_ERROR, "an MTU negotiation is in progress");
            }
            attempt.pendingMtu = reply;
        }
        if (!gatt.requestMtu(opts.optInt("mtu", 23))) {
            synchronized (attempt) {
                attempt.pendingMtu = null;
            }
            throw new BluetoothFailure(SYSTEM_ERROR, "MTU request failed");
        }
    }

    public void getBLEMTU(JSONObject opts, Reply reply) {
        String deviceId = requiredString(opts, "deviceId");
        if (rawGatt(deviceId) == null) {
            throw new BluetoothFailure(NO_CONNECTION, "no connection");
        }
        Integer mtu = negotiatedMtu.get(deviceId);
        try {
            // The ATT default until a larger one is negotiated.
            reply.ok(new JSONObject().put("mtu", mtu != null ? mtu.intValue() : 23));
        } catch (JSONException impossible) {
            throw new BluetoothFailure(SYSTEM_ERROR, "result serialisation failed");
        }
    }

    private static BluetoothGattService findService(BluetoothGatt gatt, String serviceId) {
        BluetoothGattService service;
        try {
            service = gatt.getService(UUID.fromString(serviceId));
        } catch (IllegalArgumentException notAUuid) {
            throw new BluetoothFailure(INVALID_DATA, "invalid serviceId " + serviceId);
        }
        if (service == null) {
            throw new BluetoothFailure(NO_SERVICE, "no service");
        }
        return service;
    }

    private static BluetoothGattCharacteristic findCharacteristic(
            BluetoothGatt gatt, String serviceId, String characteristicId) {
        BluetoothGattService service = findService(gatt, serviceId);
        BluetoothGattCharacteristic characteristic;
        try {
            characteristic = service.getCharacteristic(UUID.fromString(characteristicId));
        } catch (IllegalArgumentException notAUuid) {
            throw new BluetoothFailure(INVALID_DATA, "invalid characteristicId " + characteristicId);
        }
        if (characteristic == null) {
            throw new BluetoothFailure(NO_CHARACTERISTIC, "no characteristic");
        }
        return characteristic;
    }

    private static byte[] hexToBytes(String hex) {
        if (hex == null || hex.isEmpty()) return new byte[0];
        int len = hex.length();
        if (len % 2 != 0) {
            throw new BluetoothFailure(INVALID_DATA, "invalid data");
        }
        byte[] data = new byte[len / 2];
        for (int i = 0; i < len; i += 2) {
            int hi = Character.digit(hex.charAt(i), 16);
            int lo = Character.digit(hex.charAt(i + 1), 16);
            if (hi < 0 || lo < 0) {
                throw new BluetoothFailure(INVALID_DATA, "invalid data");
            }
            data[i / 2] = (byte) ((hi << 4) + lo);
        }
        return data;
    }

    // ==================== Cleanup ====================

    public synchronized void setLifecycleSuspended(boolean suspended) {
        if (suspended) {
            suspendForLifecycle();
        } else {
            resumeForLifecycle();
        }
    }

    public synchronized void suspendForLifecycle() {
        if (discoveryRequest.suspend() == LifecycleRequestState.Action.STOP) {
            stopDiscoveryInternal();
            discovering = false;
        }
        if (beaconRequest.suspend() == LifecycleRequestState.Action.STOP) {
            stopBeaconDiscoveryInternal();
            beaconDiscovering = false;
        }
    }

    public synchronized void resumeForLifecycle() {
        if (discoveryRequest.resume() == LifecycleRequestState.Action.START) {
            try {
                startDiscoveryInternal(discoveryRequest.getRequest());
                discovering = true;
            } catch (RuntimeException e) {
                Log.w(TAG, "Failed to resume BLE discovery: " + e.getMessage());
                stopDiscoveryInternal();
                discovering = false;
                discoveryRequest.startFailed(false);
                NativeMethods.onBluetoothAdapterStateChange(sessionId,
                        isAdapterEnabled(), false);
            }
        }

        if (beaconRequest.resume() == LifecycleRequestState.Action.START) {
            try {
                startBeaconDiscoveryInternal();
                beaconDiscovering = true;
            } catch (RuntimeException e) {
                Log.w(TAG, "Failed to resume beacon discovery: " + e.getMessage());
                stopBeaconDiscoveryInternal();
                beaconDiscovering = false;
                beaconRequest.startFailed(false);
                NativeMethods.onBeaconServiceChange(sessionId,
                        isAdapterEnabled(), false);
            }
        }
    }

    public synchronized void destroy() {
        discoveryRequest.destroy();
        beaconRequest.destroy();
        discovering = false;
        beaconDiscovering = false;
        ResourceCleanup.runAll(
                this::stopDiscoveryInternal,
                this::stopBeaconDiscoveryInternal,
                this::closeAdapter,
                this::finishPairings,
                discoveredBeacons::clear,
                () -> ResourceCleanup.destroyMatching(
                        gattConnections, ignored -> true,
                        attempt -> closeGatt(attempt.beginClose(), true)),
                negotiatedMtu::clear,
                cachedRssi::clear);
    }

    // ==================== Internal ====================

    @SuppressLint("MissingPermission")
    private void handleScanResult(ScanResult result) {
        BluetoothDevice device = result.getDevice();
        String address = device.getAddress();
        String name = "";
        if (hasConnectPermission()) {
            try {
                String reportedName = device.getName();
                name = reportedName != null ? reportedName : "";
            } catch (SecurityException ignored) {
                // Permission may be revoked between the check and framework call.
            }
        }

        JSONObject devJson = new JSONObject();
        try {
            devJson.put("deviceId", address);
            devJson.put("name", name);
            devJson.put("RSSI", result.getRssi());
            ScanRecord record = result.getScanRecord();
            devJson.put("advertisData", manufacturerData(record));
            devJson.put("advertisServiceUUIDs", getServiceUuids(result));
            String localName = record != null ? record.getDeviceName() : null;
            devJson.put("localName", localName != null ? localName : "");
            devJson.put("serviceData", serviceData(record));
        } catch (JSONException ignored) {}

        discoveredDevices.put(address, devJson);

        // Notify JS
        JSONArray devicesArr = new JSONArray();
        devicesArr.put(devJson);
        NativeMethods.onBluetoothDeviceFound(sessionId, devicesArr.toString());
    }

    private void handleBeaconResult(ScanResult result) {
        if (result.getScanRecord() == null || result.getScanRecord().getBytes() == null) {
            return;
        }
        byte[] scanRecord = result.getScanRecord().getBytes();
        // Parse iBeacon format: manufacturer specific data with Apple company ID (0x004C)
        JSONObject beacon = parseIBeacon(scanRecord, result.getRssi());
        if (beacon != null) {
            String key;
            try {
                key = beacon.getString("uuid") + ":" + beacon.getInt("major") + ":" + beacon.getInt("minor");
            } catch (JSONException e) {
                return;
            }
            recordBeacon(key, beacon);
        }
    }

    /**
     * Retain and publish one beacon while keeping remote-controlled state bounded.
     */
    private synchronized void recordBeacon(String key, JSONObject beacon) {
        if (!discoveredBeacons.containsKey(key)
                && discoveredBeacons.size() >= BEACON_CACHE_LIMIT) {
            return;
        }
        discoveredBeacons.put(key, beacon);

        JSONArray beaconsArr = new JSONArray();
        for (JSONObject cached : discoveredBeacons.values()) {
            beaconsArr.put(cached);
        }
        beaconUpdateReporter.update(beaconsArr.toString());
    }

    void recordBeaconForTests(String key, JSONObject beacon) {
        recordBeacon(key, beacon);
    }

    int discoveredBeaconCountForTests() {
        return discoveredBeacons.size();
    }

    private JSONObject parseIBeacon(byte[] scanRecord, int rssi) {
        // iBeacon format: ... 0xFF 0x4C 0x00 0x02 0x15 [UUID 16 bytes] [Major 2] [Minor 2] [TX 1]
        for (int i = 0; i < scanRecord.length - 25; i++) {
            if ((scanRecord[i] & 0xFF) == 0xFF
                    && (scanRecord[i + 1] & 0xFF) == 0x4C
                    && (scanRecord[i + 2] & 0xFF) == 0x00
                    && (scanRecord[i + 3] & 0xFF) == 0x02
                    && (scanRecord[i + 4] & 0xFF) == 0x15) {
                try {
                    String uuid = bytesToUuid(scanRecord, i + 5);
                    int major = ((scanRecord[i + 21] & 0xFF) << 8) | (scanRecord[i + 22] & 0xFF);
                    int minor = ((scanRecord[i + 23] & 0xFF) << 8) | (scanRecord[i + 24] & 0xFF);
                    int txPower = scanRecord[i + 25]; // signed byte

                    JSONObject beacon = new JSONObject();
                    beacon.put("uuid", uuid);
                    beacon.put("major", major);
                    beacon.put("minor", minor);
                    beacon.put("proximity", estimateProximity(rssi, txPower));
                    beacon.put("accuracy", estimateAccuracy(rssi, txPower));
                    beacon.put("rssi", rssi);
                    return beacon;
                } catch (Exception e) {
                    return null;
                }
            }
        }
        return null;
    }

    private static int estimateProximity(int rssi, int txPower) {
        double distance = estimateAccuracy(rssi, txPower);
        if (distance < 0) return 0; // unknown
        if (distance < 0.5) return 1; // immediate
        if (distance < 4.0) return 2; // near
        return 3; // far
    }

    private static double estimateAccuracy(int rssi, int txPower) {
        if (rssi == 0) return -1.0;
        double ratio = (double) rssi / txPower;
        if (ratio < 1.0) {
            return Math.pow(ratio, 10);
        }
        return 0.89976 * Math.pow(ratio, 7.7095) + 0.111;
    }

    private void registerAdapterStateReceiver() {
        if (adapterStateReceiver != null) return;
        Activity activity = getActivity();
        if (activity == null) return;
        adapterStateReceiver = new BroadcastReceiver() {
            @Override
            public void onReceive(Context context, Intent intent) {
                if (BluetoothAdapter.ACTION_STATE_CHANGED.equals(intent.getAction())) {
                    int state = intent.getIntExtra(BluetoothAdapter.EXTRA_STATE, BluetoothAdapter.ERROR);
                    boolean available = (state == BluetoothAdapter.STATE_ON);
                    NativeMethods.onBluetoothAdapterStateChange(sessionId, available, discovering);
                }
            }
        };
        IntentFilter filter = new IntentFilter(BluetoothAdapter.ACTION_STATE_CHANGED);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            activity.registerReceiver(adapterStateReceiver, filter, Context.RECEIVER_NOT_EXPORTED);
        } else {
            activity.registerReceiver(adapterStateReceiver, filter);
        }
    }

    private void unregisterAdapterStateReceiver() {
        BroadcastReceiver receiver = adapterStateReceiver;
        if (receiver == null) return;
        Activity activity = getActivity();
        if (activity == null) {
            throw new IllegalStateException("cannot unregister Bluetooth receiver without activity");
        }
        activity.unregisterReceiver(receiver);
        if (adapterStateReceiver == receiver) adapterStateReceiver = null;
    }

    /**
     * The advertisement's manufacturer-specific segment as content receives it:
     * the company identifier (little-endian, as on the air) and its data -- the
     * segment, not the whole advertisement, which is what a game parses.
     */
    private static String manufacturerData(ScanRecord record) {
        SparseArray<byte[]> segments = record != null ? record.getManufacturerSpecificData() : null;
        if (segments == null || segments.size() == 0) return "";
        int company = segments.keyAt(0);
        byte[] data = segments.valueAt(0);
        int length = data != null ? data.length : 0;
        byte[] segment = new byte[length + 2];
        segment[0] = (byte) (company & 0xFF);
        segment[1] = (byte) ((company >>> 8) & 0xFF);
        if (length > 0) System.arraycopy(data, 0, segment, 2, length);
        return bytesToHex(segment);
    }

    /** Each service's data, keyed by its UUID. */
    private static JSONObject serviceData(ScanRecord record) throws JSONException {
        JSONObject result = new JSONObject();
        Map<ParcelUuid, byte[]> data = record != null ? record.getServiceData() : null;
        if (data == null) return result;
        for (Map.Entry<ParcelUuid, byte[]> entry : data.entrySet()) {
            byte[] bytes = entry.getValue();
            result.put(entry.getKey().toString(), bytes != null ? bytesToHex(bytes) : "");
        }
        return result;
    }

    private static JSONArray getServiceUuids(ScanResult result) {
        JSONArray arr = new JSONArray();
        if (result.getScanRecord() != null && result.getScanRecord().getServiceUuids() != null) {
            for (ParcelUuid uuid : result.getScanRecord().getServiceUuids()) {
                arr.put(uuid.toString());
            }
        }
        return arr;
    }

    private static final char[] HEX_DIGITS = "0123456789abcdef".toCharArray();

    private static String bytesToHex(byte[] bytes) {
        char[] output = new char[bytes.length * 2];
        appendHex(bytes, 0, bytes.length, output, 0);
        return new String(output);
    }

    /**
     * Appends bytes without a formatter or temporary per-byte object.
     *
     * <p>BLE advertisements arrive on a callback hot path. {@code String.format}
     * parsed a formatter and allocated intermediate strings for every byte,
     * multiplying callback garbage by the advertisement length. The caller
     * owns the output buffer so the steady-state conversion itself is allocation-free.
     */
    static void appendHex(
            byte[] source, int sourceOffset, int byteCount, char[] destination, int destinationOffset) {
        for (int i = 0; i < byteCount; i++) {
            int value = source[sourceOffset + i] & 0xFF;
            destination[destinationOffset + i * 2] = HEX_DIGITS[value >>> 4];
            destination[destinationOffset + i * 2 + 1] = HEX_DIGITS[value & 0x0F];
        }
    }

    private static String bytesToUuid(byte[] bytes, int offset) {
        char[] output = new char[36];
        appendUuid(bytes, offset, output);
        return new String(output);
    }

    static void appendUuid(byte[] source, int offset, char[] destination) {
        int outputOffset = 0;
        for (int i = 0; i < 16; i++) {
            if (i == 4 || i == 6 || i == 8 || i == 10) {
                destination[outputOffset++] = '-';
            }
            int value = source[offset + i] & 0xFF;
            destination[outputOffset++] = HEX_DIGITS[value >>> 4];
            destination[outputOffset++] = HEX_DIGITS[value & 0x0F];
        }
    }
}
