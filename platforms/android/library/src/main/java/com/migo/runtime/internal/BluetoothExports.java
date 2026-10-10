package com.migo.runtime.internal;

import android.app.Activity;

import com.migo.runtime.internal.platform.BluetoothManager;

import org.json.JSONException;
import org.json.JSONObject;

import java.util.concurrent.ConcurrentHashMap;

/**
 * Domain class for per-session Bluetooth manager delegation.
 *
 * @hide
 */
public final class BluetoothExports {

    private BluetoothExports() {}

    private static final Object sBluetoothLock = new Object();

    private static final ConcurrentHashMap<Integer, BluetoothManager> sBluetoothManagers =
            new ConcurrentHashMap<>();

    private static void syncBluetoothLifecycle(int sessionId, BluetoothManager manager) {
        LifecycleStateSynchronizer.synchronize(
                manager,
                () -> NativeExports.isSessionResourceSuspended(sessionId),
                manager::setLifecycleSuspended);
    }

    private static BluetoothManager getOrCreateBluetoothManager(int sessionId) {
        BluetoothManager existing = sBluetoothManagers.get(sessionId);
        if (existing != null) {
            syncBluetoothLifecycle(sessionId, existing);
            return existing;
        }
        synchronized (sBluetoothLock) {
            existing = sBluetoothManagers.get(sessionId);
            if (existing != null) {
                syncBluetoothLifecycle(sessionId, existing);
                return existing;
            }
            if (NativeExports.isSessionTerminated(sessionId)) return null;
            RuntimeContext ctx = RuntimeRegistry.get(sessionId);
            if (ctx == null) return null;
            Activity activity = ctx.getActivity();
            if (activity == null) return null;
            boolean suspended = NativeExports.isSessionResourceSuspended(sessionId);
            BluetoothManager mgr = new BluetoothManager(sessionId, activity, suspended);
            sBluetoothManagers.put(sessionId, mgr);
            syncBluetoothLifecycle(sessionId, mgr);
            return mgr;
        }
    }

    // ==================== Requests ====================
    //
    // Every Bluetooth request is answered exactly once through
    // NativeMethods.onBluetoothResult: by the manager when the operation has
    // happened, or here with the reason it could not start. Nothing is thrown
    // back across JNI -- the runtime waits on the answer, not on the call.

    private interface Operation {
        void run(BluetoothManager manager, JSONObject options, BluetoothManager.Reply reply);
    }

    private static final BluetoothManager.ResultSink SINK = NativeMethods::onBluetoothResult;

    /**
     * Run one request. {@code opens} is whether the request may bring the
     * session's manager into being -- opening the adapter and the requests the
     * platform allows without it; any other request before then is "not init".
     * A release with nothing to release succeeds.
     */
    private static void request(int sessionId, BluetoothManager.Method method, String requestJson,
                                boolean opens, Operation operation) {
        JSONObject options;
        try {
            options = new JSONObject(requestJson);
        } catch (JSONException malformed) {
            new BluetoothManager.Reply(sessionId, method, CallbackCorrelation.ABSENT, SINK)
                    .fail(BluetoothManager.INVALID_DATA, "invalid data");
            return;
        }
        BluetoothManager.Reply reply = new BluetoothManager.Reply(
                sessionId, method, CallbackCorrelation.requestIdOf(options), SINK);
        BluetoothManager manager = opens
                ? getOrCreateBluetoothManager(sessionId)
                : sBluetoothManagers.get(sessionId);
        if (manager == null) {
            if (releases(method)) {
                reply.ok();
            } else {
                reply.fail(BluetoothManager.NOT_INIT, "not init");
            }
            return;
        }
        try {
            operation.run(manager, options, reply);
        } catch (BluetoothManager.BluetoothFailure failure) {
            reply.fail(failure.errCode, failure.getMessage());
        } catch (SecurityException denied) {
            reply.fail(BluetoothManager.SYSTEM_ERROR, denied.getMessage());
        } catch (RuntimeException unexpected) {
            reply.fail(BluetoothManager.SYSTEM_ERROR, String.valueOf(unexpected.getMessage()));
        }
    }

    private static boolean releases(BluetoothManager.Method method) {
        switch (method) {
            case CLOSE_ADAPTER:
            case STOP_DEVICES_DISCOVERY:
            case STOP_BEACON_DISCOVERY:
            case CLOSE_BLE_CONNECTION:
                return true;
            default:
                return false;
        }
    }

    public static void bluetoothOpenAdapter(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.OPEN_ADAPTER, requestJson, true,
                BluetoothManager::openAdapter);
    }

    public static void bluetoothCloseAdapter(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.CLOSE_ADAPTER, requestJson, false,
                BluetoothManager::closeAdapter);
    }

    public static void bluetoothGetAdapterState(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.GET_ADAPTER_STATE, requestJson, false,
                BluetoothManager::getAdapterState);
    }

    public static void bluetoothStartDevicesDiscovery(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.START_DEVICES_DISCOVERY, requestJson, false,
                BluetoothManager::startDiscovery);
    }

    public static void bluetoothStopDevicesDiscovery(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.STOP_DEVICES_DISCOVERY, requestJson, false,
                BluetoothManager::stopDiscovery);
    }

    public static void bluetoothGetDevices(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.GET_DEVICES, requestJson, false,
                BluetoothManager::getDevices);
    }

    public static void bluetoothGetConnectedDevices(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.GET_CONNECTED_DEVICES, requestJson, false,
                BluetoothManager::getConnectedDevices);
    }

    public static void bluetoothMakePair(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.MAKE_PAIR, requestJson, true,
                BluetoothManager::makePair);
    }

    public static void bluetoothIsDevicePaired(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.IS_DEVICE_PAIRED, requestJson, true,
                BluetoothManager::isDevicePaired);
    }

    public static void bluetoothStartBeaconDiscovery(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.START_BEACON_DISCOVERY, requestJson, true,
                BluetoothManager::startBeaconDiscovery);
    }

    public static void bluetoothStopBeaconDiscovery(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.STOP_BEACON_DISCOVERY, requestJson, false,
                BluetoothManager::stopBeaconDiscovery);
    }

    public static void bluetoothGetBeacons(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.GET_BEACONS, requestJson, false,
                BluetoothManager::getBeacons);
    }

    public static void bleCreateConnection(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.CREATE_BLE_CONNECTION, requestJson, false,
                BluetoothManager::createBLEConnection);
    }

    public static void bleCloseConnection(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.CLOSE_BLE_CONNECTION, requestJson, false,
                BluetoothManager::closeBLEConnection);
    }

    public static void bleGetDeviceServices(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.GET_BLE_DEVICE_SERVICES, requestJson, false,
                BluetoothManager::getBLEDeviceServices);
    }

    public static void bleGetDeviceCharacteristics(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.GET_BLE_DEVICE_CHARACTERISTICS, requestJson, false,
                BluetoothManager::getBLEDeviceCharacteristics);
    }

    public static void bleReadCharacteristicValue(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.READ_BLE_CHARACTERISTIC_VALUE, requestJson, false,
                BluetoothManager::readBLECharacteristicValue);
    }

    public static void bleWriteCharacteristicValue(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.WRITE_BLE_CHARACTERISTIC_VALUE, requestJson, false,
                BluetoothManager::writeBLECharacteristicValue);
    }

    public static void bleNotifyCharacteristicValueChange(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.NOTIFY_BLE_CHARACTERISTIC_VALUE_CHANGE, requestJson, false,
                BluetoothManager::notifyBLECharacteristicValueChange);
    }

    public static void bleGetDeviceRSSI(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.GET_BLE_DEVICE_RSSI, requestJson, false,
                BluetoothManager::getBLEDeviceRSSI);
    }

    public static void bleSetMTU(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.SET_BLE_MTU, requestJson, false,
                BluetoothManager::setBLEMTU);
    }

    public static void bleGetMTU(int sessionId, String requestJson) {
        request(sessionId, BluetoothManager.Method.GET_BLE_MTU, requestJson, false,
                BluetoothManager::getBLEMTU);
    }

    public static void destroyBluetoothManager(int sessionId) {
        ResourceCleanup.destroyMatching(
                sBluetoothManagers,
                id -> id == sessionId,
                BluetoothManager::destroy);
    }

    public static void suspendPowerSensitiveManagers(int sessionId) {
        BluetoothManager mgr = sBluetoothManagers.get(sessionId);
        if (mgr != null) {
            mgr.suspendForLifecycle();
        }
    }

    public static void resumePowerSensitiveManagers(int sessionId) {
        BluetoothManager mgr = sBluetoothManagers.get(sessionId);
        if (mgr != null) {
            mgr.resumeForLifecycle();
        }
    }

    public static void destroyAll(int sessionId) {
        destroyBluetoothManager(sessionId);
    }
}
