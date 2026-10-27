package com.migo.runtime.internal.platform;

import static com.migo.runtime.internal.platform.BluetoothManager.INVALID_DATA;
import static com.migo.runtime.internal.platform.BluetoothManager.NOT_AVAILABLE;
import static com.migo.runtime.internal.platform.BluetoothManager.NOT_INIT;
import static com.migo.runtime.internal.platform.BluetoothManager.NO_CHARACTERISTIC;
import static com.migo.runtime.internal.platform.BluetoothManager.NO_SERVICE;
import static com.migo.runtime.internal.platform.BluetoothManager.SYSTEM_ERROR;

import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothGatt;
import android.bluetooth.BluetoothGattCharacteristic;
import android.bluetooth.BluetoothGattDescriptor;
import android.bluetooth.BluetoothGattServerCallback;
import android.bluetooth.BluetoothGattService;
import android.bluetooth.BluetoothProfile;
import android.bluetooth.le.AdvertiseCallback;
import android.bluetooth.le.AdvertiseData;
import android.bluetooth.le.AdvertiseSettings;
import android.os.ParcelUuid;
import android.util.Log;

import com.migo.runtime.internal.platform.BluetoothManager.BluetoothFailure;
import com.migo.runtime.internal.platform.BluetoothManager.Reply;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.IdentityHashMap;
import java.util.Iterator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.function.Consumer;

/**
 * This device as a Bluetooth LE peripheral, for one session: the GATT servers
 * content makes with createBLEPeripheralServer, the services it puts on them,
 * their advertising, and what the centrals that connect do.
 *
 * <p>It exists while the session's adapter is open and closes with it
 * ({@link BluetoothManager}): its servers, their advertising, and the name the
 * device advertised under. Every request is answered once through its
 * {@link Reply}, by {@link Method} number; what centrals do is reported through
 * {@link EventSink}, by event number. Requests and the platform's callbacks are
 * serialised on this object.
 *
 * <p>A read or write request reaches content with a callbackId, which content
 * answers with writeCharacteristicValue. One it does not answer gets no
 * response, as the reference says. What content never sees is answered here:
 * the rest of a long read, from the value content gave; a long write's parts,
 * reported whole once executed; descriptors, from their values -- a
 * subscription's from who subscribed.
 */
public final class BlePeripheral {

    private static final String TAG = "BlePeripheral";

    /**
     * The peripheral requests, in the order the host-service contract numbers
     * them ({@code contracts/runtime/host-services.json}, service
     * {@code ble_peripheral}): an ordinal is the method number the runtime
     * routes the answer by.
     */
    public enum Method {
        CREATE_SERVER("createBLEPeripheralServer"),
        ADD_SERVICE("BLEPeripheralServer.addService"),
        REMOVE_SERVICE("BLEPeripheralServer.removeService"),
        START_ADVERTISING("BLEPeripheralServer.startAdvertising"),
        STOP_ADVERTISING("BLEPeripheralServer.stopAdvertising"),
        WRITE_CHARACTERISTIC_VALUE("BLEPeripheralServer.writeCharacteristicValue");

        public final String apiName;

        Method(String apiName) {
            this.apiName = apiName;
        }
    }

    /** What centrals do, by the contract's event numbers. */
    public static final int EVENT_CHARACTERISTIC_READ_REQUEST = 0;
    public static final int EVENT_CHARACTERISTIC_WRITE_REQUEST = 1;
    public static final int EVENT_CHARACTERISTIC_SUBSCRIBED = 2;
    public static final int EVENT_CHARACTERISTIC_UNSUBSCRIBED = 3;
    public static final int EVENT_CONNECTION_STATE_CHANGED = 4;

    /** Where what centrals do goes: the runtime's peripheral event callback. */
    public interface EventSink {
        void deliver(int sessionId, int event, String payloadJson);
    }

    /** One platform GATT server: what this class asks of it. */
    interface Gatt {
        boolean addService(BluetoothGattService service);

        boolean removeService(BluetoothGattService service);

        boolean sendResponse(BluetoothDevice device, int requestId, int status, int offset, byte[] value);

        /** One notification or indication, reported sent by onNotificationSent. */
        boolean sendNotification(BluetoothDevice device, BluetoothGattCharacteristic characteristic,
                       boolean confirm, byte[] value);

        void close();
    }

    /** What this class asks of the device's radio. */
    interface Radio {
        /** A GATT server reporting to {@code callback}; null when none opens. */
        Gatt openServer(BluetoothGattServerCallback callback);

        /** Whether this device can advertise at all. */
        boolean advertisingSupported();

        /** Whether the app may advertise. */
        boolean advertisingPermitted();

        void startAdvertising(AdvertiseSettings settings, AdvertiseData data, AdvertiseData scanResponse,
                              AdvertiseCallback callback);

        void stopAdvertising(AdvertiseCallback callback);

        /** The name the device advertises under. */
        String name();

        /** Advertise under {@code name} from now on; {@code done} hears whether it took. */
        void rename(String name, Consumer<Boolean> done);
    }

    /** The longest attribute value ATT carries. */
    static final int MAX_VALUE_BYTES = 512;
    /** The ATT MTU a connection starts with. */
    private static final int DEFAULT_MTU = 23;
    /** The descriptor a central subscribes through. */
    static final UUID CCCD = UUID.fromString("00002902-0000-1000-8000-00805f9b34fb");
    /** Bluetooth's base UUID, which a 16- or 32-bit UUID abbreviates. */
    private static final String BASE_UUID_TAIL = "-0000-1000-8000-00805f9b34fb";
    /** The company an iBeacon advertises under. */
    private static final int APPLE_COMPANY_ID = 0x004C;
    /** An iBeacon's calibrated power at one metre when content gives none. */
    private static final int DEFAULT_MEASURED_POWER = -59;
    private static final int CCCD_NOTIFY = 0x01;
    private static final int CCCD_INDICATE = 0x02;

    private final int sessionId;
    private final Radio radio;
    private final EventSink events;
    private final Map<Integer, Server> servers = new HashMap<>();
    private int nextCallbackId = 1;
    /** The device's own name while servers advertise under another; null otherwise. */
    private String ownName;
    private boolean closed;

    BlePeripheral(int sessionId, Radio radio, EventSink events) {
        this.sessionId = sessionId;
        this.radio = radio;
        this.events = events;
    }

    // ==================== Requests ====================

    public synchronized void createServer(JSONObject opts, Reply reply) {
        int id = serverId(opts);
        if (servers.containsKey(id)) {
            throw new BluetoothFailure(INVALID_DATA, "server " + id + " exists");
        }
        Server server = new Server(id);
        Gatt gatt = radio.openServer(server);
        if (gatt == null) throw new BluetoothFailure(SYSTEM_ERROR, "no GATT server opened");
        server.gatt = gatt;
        servers.put(id, server);
        reply.ok();
    }

    public synchronized void addService(JSONObject opts, Reply reply) {
        Server server = server(opts);
        JSONObject described = opts.optJSONObject("service");
        if (described == null) throw new BluetoothFailure(INVALID_DATA, "service is required");
        Service service = Service.parse(described);
        if (server.services.containsKey(service.uuid) || server.adding(service.uuid)) {
            throw new BluetoothFailure(SYSTEM_ERROR, "service " + service.name + " already added");
        }
        server.adding.add(new Adding(service, reply));
        if (server.adding.size() == 1) server.addNext();
    }

    public synchronized void removeService(JSONObject opts, Reply reply) {
        Server server = server(opts);
        Service service = server.services.get(uuid(required(opts, "serviceId")));
        if (service == null) throw new BluetoothFailure(NO_SERVICE, "no service");
        if (!server.gatt.removeService(service.gatt)) {
            throw new BluetoothFailure(SYSTEM_ERROR, "service not removed");
        }
        server.forget(service);
        reply.ok();
    }

    public synchronized void startAdvertising(JSONObject opts, Reply reply) {
        Server server = server(opts);
        if (!radio.advertisingSupported()) {
            throw new BluetoothFailure(NOT_AVAILABLE, "this device cannot advertise");
        }
        if (!radio.advertisingPermitted()) {
            throw new BluetoothFailure(SYSTEM_ERROR, "permission denied (BLUETOOTH_ADVERTISE)");
        }
        Advertisement advertisement = Advertisement.parse(opts);
        server.stopAdvertising("replaced by a later startAdvertising");
        Advertising advertising = new Advertising(server, advertisement, reply);
        server.advertising = advertising;
        String name = advertisement.deviceName;
        if (name == null || name.equals(radio.name())) {
            advertising.start();
            return;
        }
        if (ownName == null) ownName = radio.name();
        radio.rename(name, took -> {
            synchronized (BlePeripheral.this) {
                if (server.advertising != advertising) return;
                if (took) {
                    advertising.start();
                } else {
                    server.advertising = null;
                    restoreNameIfUnused();
                    reply.fail(SYSTEM_ERROR, "deviceName not taken");
                }
            }
        });
    }

    public synchronized void stopAdvertising(JSONObject opts, Reply reply) {
        // A server that is gone advertises nothing.
        Server server = servers.get(opts.optInt("serverId", 0));
        if (server != null) server.stopAdvertising("stopped");
        reply.ok();
    }

    public synchronized void writeCharacteristicValue(JSONObject opts, Reply reply) {
        Server server = server(opts);
        Service service = server.services.get(uuid(required(opts, "serviceId")));
        if (service == null) throw new BluetoothFailure(NO_SERVICE, "no service");
        Characteristic characteristic =
                service.characteristics.get(uuid(required(opts, "characteristicId")));
        if (characteristic == null) throw new BluetoothFailure(NO_CHARACTERISTIC, "no characteristic");
        byte[] value = value(opts.optString("value", ""));
        characteristic.value = value;
        if (opts.has("callbackId")) server.answer(opts.optInt("callbackId"), value);
        if (opts.optBoolean("needNotify", false)) {
            server.notifySubscribers(characteristic, value, reply);
        } else {
            reply.ok();
        }
    }

    /** Close every server: the adapter closed or the session ended. */
    public synchronized void close() {
        if (closed) return;
        closed = true;
        List<Server> open = new ArrayList<>(servers.values());
        servers.clear();
        for (Server server : open) {
            try {
                server.close();
            } catch (RuntimeException failure) {
                Log.w(TAG, "closing a GATT server failed", failure);
            }
        }
        restoreNameIfUnused();
    }

    // ==================== Parsing ====================

    private static int serverId(JSONObject opts) {
        int id = opts.optInt("serverId", 0);
        if (id <= 0) throw new BluetoothFailure(INVALID_DATA, "serverId is required");
        return id;
    }

    private Server server(JSONObject opts) {
        Server server = servers.get(serverId(opts));
        if (server == null) throw new BluetoothFailure(SYSTEM_ERROR, "server closed");
        return server;
    }

    private static String required(JSONObject opts, String name) {
        String value = opts.optString(name, "");
        if (value.isEmpty()) throw new BluetoothFailure(INVALID_DATA, name + " is required");
        return value;
    }

    /** A UUID as content names one: 16 or 32 bits of the base UUID, or all 128. */
    static UUID uuid(String text) {
        try {
            switch (text.length()) {
                case 4:
                    Integer.parseInt(text, 16);
                    return UUID.fromString("0000" + text + BASE_UUID_TAIL);
                case 8:
                    Long.parseLong(text, 16);
                    return UUID.fromString(text + BASE_UUID_TAIL);
                case 36:
                    return UUID.fromString(text);
                default:
                    break;
            }
        } catch (IllegalArgumentException malformed) {
            // Reported below.
        }
        throw new BluetoothFailure(INVALID_DATA, "invalid UUID " + text);
    }

    /** A value content gave, as hex: at most what one attribute holds. */
    static byte[] value(String hex) {
        if (hex.length() > MAX_VALUE_BYTES * 2) {
            throw new BluetoothFailure(INVALID_DATA, "value longer than " + MAX_VALUE_BYTES + " bytes");
        }
        return BluetoothManager.hexToBytes(hex);
    }

    // ==================== Services ====================

    /** A service content added, its parts named as content named them. */
    static final class Service {
        final String name;
        final UUID uuid;
        final BluetoothGattService gatt;
        final Map<UUID, Characteristic> characteristics = new LinkedHashMap<>();

        private Service(String name, UUID uuid) {
            this.name = name;
            this.uuid = uuid;
            this.gatt = new BluetoothGattService(uuid, BluetoothGattService.SERVICE_TYPE_PRIMARY);
        }

        static Service parse(JSONObject described) {
            String name = required(described, "uuid");
            Service service = new Service(name, uuid(name));
            JSONArray characteristics = described.optJSONArray("characteristics");
            if (characteristics == null) {
                throw new BluetoothFailure(INVALID_DATA, "characteristics is required");
            }
            for (int i = 0; i < characteristics.length(); i++) {
                JSONObject c = characteristics.optJSONObject(i);
                if (c == null) throw new BluetoothFailure(INVALID_DATA, "invalid characteristic");
                Characteristic characteristic = Characteristic.parse(service, c);
                if (service.characteristics.put(characteristic.uuid, characteristic) != null) {
                    throw new BluetoothFailure(INVALID_DATA, "characteristic " + characteristic.name + " twice");
                }
                service.gatt.addCharacteristic(characteristic.gatt);
            }
            return service;
        }
    }

    /** A characteristic, its value and its descriptors' values held here. */
    static final class Characteristic {
        final Service service;
        final String name;
        final UUID uuid;
        final BluetoothGattCharacteristic gatt;
        final Map<UUID, byte[]> descriptors = new HashMap<>();
        byte[] value;

        private Characteristic(Service service, String name, UUID uuid, int properties, int permissions,
                               byte[] value) {
            this.service = service;
            this.name = name;
            this.uuid = uuid;
            this.gatt = new BluetoothGattCharacteristic(uuid, properties, permissions);
            this.value = value;
        }

        static Characteristic parse(Service service, JSONObject c) {
            String name = required(c, "uuid");
            JSONObject properties = c.optJSONObject("properties");
            JSONObject permission = c.optJSONObject("permission");
            int props = flag(properties, "write", BluetoothGattCharacteristic.PROPERTY_WRITE)
                    | flag(properties, "writeNoResponse", BluetoothGattCharacteristic.PROPERTY_WRITE_NO_RESPONSE)
                    | flag(properties, "read", BluetoothGattCharacteristic.PROPERTY_READ)
                    | flag(properties, "notify", BluetoothGattCharacteristic.PROPERTY_NOTIFY)
                    | flag(properties, "indicate", BluetoothGattCharacteristic.PROPERTY_INDICATE);
            int perms = flag(permission, "readable", BluetoothGattCharacteristic.PERMISSION_READ)
                    | flag(permission, "writeable", BluetoothGattCharacteristic.PERMISSION_WRITE)
                    | flag(permission, "readEncryptionRequired",
                            BluetoothGattCharacteristic.PERMISSION_READ_ENCRYPTED)
                    | flag(permission, "writeEncryptionRequired",
                            BluetoothGattCharacteristic.PERMISSION_WRITE_ENCRYPTED);
            Characteristic characteristic = new Characteristic(
                    service, name, uuid(name), props, perms, value(c.optString("value", "")));
            boolean subscribable = (props & (BluetoothGattCharacteristic.PROPERTY_NOTIFY
                    | BluetoothGattCharacteristic.PROPERTY_INDICATE)) != 0;
            boolean subscription = false;
            JSONArray descriptors = c.optJSONArray("descriptors");
            for (int i = 0; descriptors != null && i < descriptors.length(); i++) {
                JSONObject d = descriptors.optJSONObject(i);
                if (d == null) throw new BluetoothFailure(INVALID_DATA, "invalid descriptor");
                UUID uuid = uuid(required(d, "uuid"));
                JSONObject dp = d.optJSONObject("permission");
                int descriptorPerms = flag(dp, "read", BluetoothGattDescriptor.PERMISSION_READ)
                        | flag(dp, "write", BluetoothGattDescriptor.PERMISSION_WRITE);
                if (CCCD.equals(uuid)) {
                    // Who subscribed is the value: a central reads and writes its own.
                    subscription = true;
                    descriptorPerms = BluetoothGattDescriptor.PERMISSION_READ
                            | BluetoothGattDescriptor.PERMISSION_WRITE;
                } else {
                    characteristic.descriptors.put(uuid, value(d.optString("value", "")));
                }
                characteristic.gatt.addDescriptor(new BluetoothGattDescriptor(uuid, descriptorPerms));
            }
            if (subscribable && !subscription) {
                // A central subscribes through this descriptor: one content left out
                // would leave notify and indicate unusable.
                characteristic.gatt.addDescriptor(new BluetoothGattDescriptor(CCCD,
                        BluetoothGattDescriptor.PERMISSION_READ | BluetoothGattDescriptor.PERMISSION_WRITE));
            }
            return characteristic;
        }

        private static int flag(JSONObject flags, String name, int bit) {
            return flags != null && flags.optBoolean(name, false) ? bit : 0;
        }
    }

    /** A service waiting for the platform to serve it: one at a time per server. */
    private static final class Adding {
        final Service service;
        final Reply reply;

        Adding(Service service, Reply reply) {
            this.service = service;
            this.reply = reply;
        }
    }

    // ==================== Advertising ====================

    /** What content asked to advertise, as the platform takes it. */
    static final class Advertisement {
        final boolean connectable;
        final String deviceName;
        final int txPower;
        final AdvertiseData data;

        private Advertisement(boolean connectable, String deviceName, int txPower, AdvertiseData data) {
            this.connectable = connectable;
            this.deviceName = deviceName;
            this.txPower = txPower;
            this.data = data;
        }

        static Advertisement parse(JSONObject opts) {
            JSONObject request = opts.optJSONObject("advertiseRequest");
            if (request == null) throw new BluetoothFailure(INVALID_DATA, "advertiseRequest is required");
            AdvertiseData.Builder data = new AdvertiseData.Builder()
                    .setIncludeDeviceName(false)
                    .setIncludeTxPowerLevel(false);
            JSONArray uuids = request.optJSONArray("serviceUuids");
            for (int i = 0; uuids != null && i < uuids.length(); i++) {
                data.addServiceUuid(new ParcelUuid(uuid(uuids.optString(i, ""))));
            }
            JSONArray manufacturers = request.optJSONArray("manufacturerData");
            for (int i = 0; manufacturers != null && i < manufacturers.length(); i++) {
                JSONObject m = manufacturers.optJSONObject(i);
                if (m == null) throw new BluetoothFailure(INVALID_DATA, "invalid manufacturerData");
                data.addManufacturerData(manufacturerId(required(m, "manufacturerId")),
                        value(m.optString("manufacturerSpecificData", "")));
            }
            JSONObject beacon = request.optJSONObject("beacon");
            if (beacon != null) {
                data.addManufacturerData(APPLE_COMPANY_ID, iBeacon(beacon));
            }
            String name = request.optString("deviceName", "");
            return new Advertisement(
                    request.optBoolean("connectable", true),
                    name.isEmpty() ? null : name,
                    txPower(opts.optString("powerLevel", "medium")),
                    data.build());
        }

        /** The name, in the scan response: the advertisement keeps its room for the rest. */
        AdvertiseData scanResponse() {
            return deviceName == null ? null : new AdvertiseData.Builder().setIncludeDeviceName(true).build();
        }

        private static int manufacturerId(String text) {
            if (!text.startsWith("0x") && !text.startsWith("0X")) {
                throw new BluetoothFailure(INVALID_DATA, "manufacturerId must be 0x and hex");
            }
            try {
                int id = Integer.parseInt(text.substring(2), 16);
                if (id <= 0xFFFF) return id;
            } catch (NumberFormatException malformed) {
                // Reported below.
            }
            throw new BluetoothFailure(INVALID_DATA, "manufacturerId must be 0x and hex");
        }

        private static int txPower(String level) {
            switch (level) {
                case "low":
                    return AdvertiseSettings.ADVERTISE_TX_POWER_LOW;
                case "high":
                    return AdvertiseSettings.ADVERTISE_TX_POWER_HIGH;
                default:
                    return AdvertiseSettings.ADVERTISE_TX_POWER_MEDIUM;
            }
        }
    }

    /**
     * An iBeacon's manufacturer data: its type and length, the proximity UUID,
     * major and minor big-endian, and the calibrated power at one metre.
     */
    static byte[] iBeacon(JSONObject beacon) {
        UUID uuid = uuid(required(beacon, "uuid"));
        int major = beacon.optInt("major", -1);
        int minor = beacon.optInt("minor", -1);
        if (major < 0 || major > 0xFFFF || minor < 0 || minor > 0xFFFF) {
            throw new BluetoothFailure(INVALID_DATA, "beacon major and minor are 0 to 65535");
        }
        int power = beacon.optInt("measuredPower", DEFAULT_MEASURED_POWER);
        byte[] data = new byte[23];
        data[0] = 0x02;
        data[1] = 0x15;
        long high = uuid.getMostSignificantBits();
        long low = uuid.getLeastSignificantBits();
        for (int i = 0; i < 8; i++) {
            data[2 + i] = (byte) (high >>> (56 - 8 * i));
            data[10 + i] = (byte) (low >>> (56 - 8 * i));
        }
        data[18] = (byte) (major >>> 8);
        data[19] = (byte) major;
        data[20] = (byte) (minor >>> 8);
        data[21] = (byte) minor;
        data[22] = (byte) power;
        return data;
    }

    /** One server's advertising, from the request to its stop. */
    private final class Advertising {
        final Server server;
        final Advertisement advertisement;
        final Reply reply;
        final AdvertiseCallback callback = new AdvertiseCallback() {
            @Override
            public void onStartSuccess(AdvertiseSettings settingsInEffect) {
                synchronized (BlePeripheral.this) {
                    if (server.advertising == Advertising.this) reply.ok();
                }
            }

            @Override
            public void onStartFailure(int errorCode) {
                synchronized (BlePeripheral.this) {
                    if (server.advertising != Advertising.this) return;
                    server.advertising = null;
                    restoreNameIfUnused();
                    reply.fail(SYSTEM_ERROR, advertiseFailure(errorCode));
                }
            }
        };
        boolean started;

        Advertising(Server server, Advertisement advertisement, Reply reply) {
            this.server = server;
            this.advertisement = advertisement;
            this.reply = reply;
        }

        void start() {
            AdvertiseSettings settings = new AdvertiseSettings.Builder()
                    .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_BALANCED)
                    .setTxPowerLevel(advertisement.txPower)
                    .setConnectable(advertisement.connectable)
                    .setTimeout(0)
                    .build();
            try {
                radio.startAdvertising(settings, advertisement.data, advertisement.scanResponse(), callback);
                started = true;
            } catch (RuntimeException failure) {
                server.advertising = null;
                restoreNameIfUnused();
                reply.fail(SYSTEM_ERROR, String.valueOf(failure.getMessage()));
            }
        }

        void stop(String reason) {
            if (started) radio.stopAdvertising(callback);
            reply.fail(SYSTEM_ERROR, reason);
        }
    }

    static String advertiseFailure(int errorCode) {
        switch (errorCode) {
            case AdvertiseCallback.ADVERTISE_FAILED_DATA_TOO_LARGE:
                return "advertising data too large";
            case AdvertiseCallback.ADVERTISE_FAILED_TOO_MANY_ADVERTISERS:
                return "too many advertisers";
            case AdvertiseCallback.ADVERTISE_FAILED_ALREADY_STARTED:
                return "already advertising";
            case AdvertiseCallback.ADVERTISE_FAILED_FEATURE_UNSUPPORTED:
                return "advertising not supported";
            default:
                return "advertising failed (" + errorCode + ")";
        }
    }

    /**
     * The device's own name back once no server advertises under another. The
     * platform advertises only the adapter's name, so a server advertising a
     * deviceName lends it that name -- one at a time, the latest.
     */
    private void restoreNameIfUnused() {
        if (ownName == null) return;
        for (Server server : servers.values()) {
            Advertising advertising = server.advertising;
            if (advertising != null && advertising.advertisement.deviceName != null) return;
        }
        String name = ownName;
        ownName = null;
        radio.rename(name, took -> {
            if (!took) Log.w(TAG, "the device's own Bluetooth name was not restored");
        });
    }

    // ==================== Servers ====================

    private int nextCallbackId() {
        int id = nextCallbackId;
        nextCallbackId = id == Integer.MAX_VALUE ? 1 : id + 1;
        return id;
    }

    private void emit(int event, JSONObject payload) {
        events.deliver(sessionId, event, payload.toString());
    }

    /** A request a central waits on: answered by content's callbackId. */
    private static final class Request {
        final BluetoothDevice device;
        final int requestId;
        final int offset;

        Request(BluetoothDevice device, int requestId, int offset) {
            this.device = device;
            this.requestId = requestId;
            this.offset = offset;
        }
    }

    /** One notification or indication to one central. */
    private static final class Notification {
        final BluetoothDevice device;
        final BluetoothGattCharacteristic characteristic;
        final boolean confirm;
        final byte[] value;
        final Ticket ticket;

        Notification(BluetoothDevice device, BluetoothGattCharacteristic characteristic, boolean confirm,
                     byte[] value, Ticket ticket) {
            this.device = device;
            this.characteristic = characteristic;
            this.confirm = confirm;
            this.value = value;
            this.ticket = ticket;
        }
    }

    /**
     * One writeCharacteristicValue's notifications: answered once each has
     * been sent. One to a central that left is not content's failure.
     */
    private static final class Ticket {
        final Reply reply;
        int remaining;
        boolean failed;

        Ticket(Reply reply, int remaining) {
            this.reply = reply;
            this.remaining = remaining;
        }

        void done(boolean sent) {
            if (!sent) failed = true;
            if (--remaining > 0) return;
            if (failed) {
                reply.fail(SYSTEM_ERROR, "notification not sent");
            } else {
                reply.ok();
            }
        }
    }

    /** One GATT server content made, and what its centrals do. */
    final class Server extends BluetoothGattServerCallback {
        final int id;
        Gatt gatt;
        final Map<UUID, Service> services = new HashMap<>();
        final IdentityHashMap<BluetoothGattCharacteristic, Characteristic> byGatt = new IdentityHashMap<>();
        final ArrayDeque<Adding> adding = new ArrayDeque<>();
        Advertising advertising;
        final Map<String, BluetoothDevice> connected = new HashMap<>();
        final Map<String, Integer> mtus = new HashMap<>();
        /** Each central's subscriptions, in the order they came: CCCD bits by characteristic. */
        final Map<String, Map<Characteristic, Integer>> subscriptions = new LinkedHashMap<>();
        /** Each central's long writes, until executed. */
        final Map<String, Map<Characteristic, byte[]>> prepared = new HashMap<>();
        final Map<Integer, Request> requests = new HashMap<>();
        final ArrayDeque<Notification> notifications = new ArrayDeque<>();
        Notification sending;

        Server(int id) {
            this.id = id;
        }

        boolean adding(UUID uuid) {
            for (Adding pending : adding) {
                if (pending.service.uuid.equals(uuid)) return true;
            }
            return false;
        }

        void addNext() {
            while (!adding.isEmpty()) {
                Adding head = adding.peek();
                boolean started;
                try {
                    started = gatt.addService(head.service.gatt);
                } catch (RuntimeException failure) {
                    started = false;
                }
                if (started) return;
                adding.poll();
                head.reply.fail(SYSTEM_ERROR, "service not added");
            }
        }

        void forget(Service service) {
            services.remove(service.uuid);
            for (Characteristic characteristic : service.characteristics.values()) {
                byGatt.remove(characteristic.gatt);
                for (Map<Characteristic, Integer> subscribed : subscriptions.values()) {
                    subscribed.remove(characteristic);
                }
                for (Map<Characteristic, byte[]> writes : prepared.values()) {
                    writes.remove(characteristic);
                }
            }
        }

        void stopAdvertising(String reason) {
            Advertising current = advertising;
            if (current == null) return;
            advertising = null;
            try {
                current.stop(reason);
            } finally {
                restoreNameIfUnused();
            }
        }

        /** Content's answer to request {@code callbackId}, if one still waits. */
        void answer(int callbackId, byte[] value) {
            Request request = requests.remove(callbackId);
            if (request == null) return;
            byte[] response = request.offset < 0
                    ? slice(value, 0, mtu(request.device) - 1)
                    : value;
            gatt.sendResponse(request.device, request.requestId, BluetoothGatt.GATT_SUCCESS,
                    Math.max(request.offset, 0), response);
        }

        void notifySubscribers(Characteristic characteristic, byte[] value, Reply reply) {
            List<Notification> outgoing = new ArrayList<>();
            Ticket ticket = new Ticket(reply, 0);
            for (Map.Entry<String, Map<Characteristic, Integer>> entry : subscriptions.entrySet()) {
                Integer bits = entry.getValue().get(characteristic);
                BluetoothDevice device = connected.get(entry.getKey());
                if (bits == null || device == null) continue;
                // A notification where the central takes either: it waits on nothing.
                boolean confirm = (bits & CCCD_NOTIFY) == 0;
                outgoing.add(new Notification(device, characteristic.gatt, confirm,
                        slice(value, 0, mtu(device) - 3), ticket));
            }
            if (outgoing.isEmpty()) {
                reply.ok();
                return;
            }
            ticket.remaining = outgoing.size();
            notifications.addAll(outgoing);
            pump();
        }

        /** The next notification, once the one before it was sent. */
        private void pump() {
            while (sending == null && !notifications.isEmpty()) {
                Notification next = notifications.poll();
                boolean started;
                try {
                    started = gatt.sendNotification(next.device, next.characteristic, next.confirm, next.value);
                } catch (RuntimeException failure) {
                    started = false;
                }
                if (started) {
                    sending = next;
                } else {
                    next.ticket.done(false);
                }
            }
        }

        private int mtu(BluetoothDevice device) {
            Integer mtu = mtus.get(device.getAddress());
            return mtu != null ? mtu : DEFAULT_MTU;
        }

        /** A central gone: what it asked, subscribed to and was being sent. */
        private void forgetDevice(String address) {
            mtus.remove(address);
            subscriptions.remove(address);
            prepared.remove(address);
            requests.values().removeIf(request -> address.equals(request.device.getAddress()));
            Iterator<Notification> queued = notifications.iterator();
            while (queued.hasNext()) {
                Notification notification = queued.next();
                if (address.equals(notification.device.getAddress())) {
                    queued.remove();
                    notification.ticket.done(true);
                }
            }
            if (sending != null && address.equals(sending.device.getAddress())) {
                Notification gone = sending;
                sending = null;
                gone.ticket.done(true);
                pump();
            }
        }

        void close() {
            stopAdvertising("adapter closed");
            for (Adding pending : adding) pending.reply.fail(NOT_INIT, "adapter closed");
            adding.clear();
            for (Notification queued : notifications) queued.ticket.done(false);
            notifications.clear();
            if (sending != null) {
                sending.ticket.done(false);
                sending = null;
            }
            requests.clear();
            subscriptions.clear();
            prepared.clear();
            connected.clear();
            gatt.close();
        }

        private JSONObject payload(Characteristic characteristic) {
            JSONObject payload = new JSONObject();
            try {
                payload.put("serverId", id);
                payload.put("serviceId", characteristic.service.name);
                payload.put("characteristicId", characteristic.name);
            } catch (JSONException impossible) {
                // Only strings and ints are put.
            }
            return payload;
        }

        private void respond(BluetoothDevice device, int requestId, int status, int offset, byte[] value) {
            try {
                gatt.sendResponse(device, requestId, status, offset, value);
            } catch (RuntimeException failure) {
                Log.w(TAG, "a response to a central was not sent", failure);
            }
        }

        /** A read from {@code offset}: as much as one response carries. */
        private void respondRead(BluetoothDevice device, int requestId, int offset, byte[] value) {
            if (offset > value.length) {
                respond(device, requestId, BluetoothGatt.GATT_INVALID_OFFSET, offset, new byte[0]);
                return;
            }
            respond(device, requestId, BluetoothGatt.GATT_SUCCESS, offset,
                    slice(value, offset, mtu(device) - 1));
        }

        // ==================== Platform callbacks ====================

        @Override
        public void onServiceAdded(int status, BluetoothGattService service) {
            synchronized (BlePeripheral.this) {
                Adding head = adding.poll();
                if (head == null) return;
                if (status == BluetoothGatt.GATT_SUCCESS) {
                    services.put(head.service.uuid, head.service);
                    for (Characteristic characteristic : head.service.characteristics.values()) {
                        byGatt.put(characteristic.gatt, characteristic);
                    }
                    head.reply.ok();
                } else {
                    head.reply.fail(SYSTEM_ERROR, "service not added (status " + status + ")");
                }
                addNext();
            }
        }

        @Override
        public void onConnectionStateChange(BluetoothDevice device, int status, int newState) {
            synchronized (BlePeripheral.this) {
                if (closed || servers.get(id) != this) return;
                String address = device.getAddress();
                boolean connected = newState == BluetoothProfile.STATE_CONNECTED;
                if (connected) {
                    if (this.connected.put(address, device) != null) return;
                } else {
                    if (this.connected.remove(address) == null) return;
                    forgetDevice(address);
                }
                JSONObject payload = new JSONObject();
                try {
                    payload.put("serverId", id);
                    payload.put("deviceId", address);
                    payload.put("connected", connected);
                } catch (JSONException impossible) {
                    // Only strings, ints and booleans are put.
                }
                emit(EVENT_CONNECTION_STATE_CHANGED, payload);
            }
        }

        @Override
        public void onMtuChanged(BluetoothDevice device, int mtu) {
            synchronized (BlePeripheral.this) {
                mtus.put(device.getAddress(), mtu);
            }
        }

        @Override
        public void onCharacteristicReadRequest(BluetoothDevice device, int requestId, int offset,
                                                BluetoothGattCharacteristic gattCharacteristic) {
            synchronized (BlePeripheral.this) {
                Characteristic characteristic = byGatt.get(gattCharacteristic);
                if (closed || characteristic == null) {
                    respond(device, requestId, BluetoothGatt.GATT_FAILURE, offset, new byte[0]);
                    return;
                }
                if (offset > 0) {
                    // The rest of a long read, from the value content gave.
                    respondRead(device, requestId, offset, characteristic.value);
                    return;
                }
                int callbackId = nextCallbackId();
                // A read's answer carries the value from its start: offset -1.
                requests.put(callbackId, new Request(device, requestId, -1));
                JSONObject payload = payload(characteristic);
                try {
                    payload.put("callbackId", callbackId);
                } catch (JSONException impossible) {
                    // Only ints are put.
                }
                emit(EVENT_CHARACTERISTIC_READ_REQUEST, payload);
            }
        }

        @Override
        public void onCharacteristicWriteRequest(BluetoothDevice device, int requestId,
                                                 BluetoothGattCharacteristic gattCharacteristic,
                                                 boolean preparedWrite, boolean responseNeeded,
                                                 int offset, byte[] written) {
            synchronized (BlePeripheral.this) {
                byte[] value = written != null ? written : new byte[0];
                Characteristic characteristic = byGatt.get(gattCharacteristic);
                if (closed || characteristic == null) {
                    if (responseNeeded) respond(device, requestId, BluetoothGatt.GATT_FAILURE, offset, value);
                    return;
                }
                String address = device.getAddress();
                if (preparedWrite) {
                    // A long write's part: held until the central executes it.
                    if (offset < 0 || offset + value.length > MAX_VALUE_BYTES) {
                        if (responseNeeded) {
                            respond(device, requestId, BluetoothGatt.GATT_INVALID_ATTRIBUTE_LENGTH, offset, value);
                        }
                        return;
                    }
                    Map<Characteristic, byte[]> writes =
                            prepared.computeIfAbsent(address, ignored -> new LinkedHashMap<>());
                    byte[] held = writes.get(characteristic);
                    byte[] grown = held == null
                            ? new byte[offset + value.length]
                            : Arrays.copyOf(held, Math.max(held.length, offset + value.length));
                    System.arraycopy(value, 0, grown, offset, value.length);
                    writes.put(characteristic, grown);
                    if (responseNeeded) respond(device, requestId, BluetoothGatt.GATT_SUCCESS, offset, value);
                    return;
                }
                int callbackId = nextCallbackId();
                if (responseNeeded) requests.put(callbackId, new Request(device, requestId, offset));
                emit(EVENT_CHARACTERISTIC_WRITE_REQUEST, writePayload(characteristic, callbackId, value));
            }
        }

        private JSONObject writePayload(Characteristic characteristic, int callbackId, byte[] value) {
            JSONObject payload = payload(characteristic);
            try {
                payload.put("callbackId", callbackId);
                payload.put("value", BluetoothManager.bytesToHex(value));
            } catch (JSONException impossible) {
                // Only ints and strings are put.
            }
            return payload;
        }

        @Override
        public void onExecuteWrite(BluetoothDevice device, int requestId, boolean execute) {
            synchronized (BlePeripheral.this) {
                Map<Characteristic, byte[]> writes = prepared.remove(device.getAddress());
                if (!closed && execute && writes != null) {
                    // Whole, as content would have had it in one write; the
                    // execution itself is answered here.
                    for (Map.Entry<Characteristic, byte[]> write : writes.entrySet()) {
                        emit(EVENT_CHARACTERISTIC_WRITE_REQUEST,
                                writePayload(write.getKey(), nextCallbackId(), write.getValue()));
                    }
                }
                respond(device, requestId, BluetoothGatt.GATT_SUCCESS, 0, new byte[0]);
            }
        }

        @Override
        public void onDescriptorReadRequest(BluetoothDevice device, int requestId, int offset,
                                            BluetoothGattDescriptor descriptor) {
            synchronized (BlePeripheral.this) {
                Characteristic characteristic = byGatt.get(descriptor.getCharacteristic());
                if (closed || characteristic == null) {
                    respond(device, requestId, BluetoothGatt.GATT_FAILURE, offset, new byte[0]);
                    return;
                }
                byte[] value;
                if (CCCD.equals(descriptor.getUuid())) {
                    Map<Characteristic, Integer> subscribed = subscriptions.get(device.getAddress());
                    Integer bits = subscribed != null ? subscribed.get(characteristic) : null;
                    value = new byte[] {(byte) (bits != null ? bits : 0), 0};
                } else {
                    byte[] held = characteristic.descriptors.get(descriptor.getUuid());
                    value = held != null ? held : new byte[0];
                }
                respondRead(device, requestId, offset, value);
            }
        }

        @Override
        public void onDescriptorWriteRequest(BluetoothDevice device, int requestId,
                                             BluetoothGattDescriptor descriptor, boolean preparedWrite,
                                             boolean responseNeeded, int offset, byte[] written) {
            synchronized (BlePeripheral.this) {
                byte[] value = written != null ? written : new byte[0];
                Characteristic characteristic = byGatt.get(descriptor.getCharacteristic());
                int status;
                if (closed || characteristic == null) {
                    status = BluetoothGatt.GATT_FAILURE;
                } else if (preparedWrite) {
                    status = BluetoothGatt.GATT_REQUEST_NOT_SUPPORTED;
                } else if (offset != 0) {
                    status = BluetoothGatt.GATT_INVALID_OFFSET;
                } else if (value.length > MAX_VALUE_BYTES) {
                    status = BluetoothGatt.GATT_INVALID_ATTRIBUTE_LENGTH;
                } else if (CCCD.equals(descriptor.getUuid())) {
                    subscribe(device.getAddress(), characteristic,
                            value.length > 0 ? value[0] & (CCCD_NOTIFY | CCCD_INDICATE) : 0);
                    status = BluetoothGatt.GATT_SUCCESS;
                } else {
                    characteristic.descriptors.put(descriptor.getUuid(), value);
                    status = BluetoothGatt.GATT_SUCCESS;
                }
                if (responseNeeded) respond(device, requestId, status, offset, value);
            }
        }

        private void subscribe(String address, Characteristic characteristic, int bits) {
            Map<Characteristic, Integer> subscribed =
                    subscriptions.computeIfAbsent(address, ignored -> new HashMap<>());
            Integer before = bits != 0 ? subscribed.put(characteristic, bits) : subscribed.remove(characteristic);
            if (subscribed.isEmpty()) subscriptions.remove(address);
            if ((before == null) == (bits == 0)) return;
            emit(bits != 0 ? EVENT_CHARACTERISTIC_SUBSCRIBED : EVENT_CHARACTERISTIC_UNSUBSCRIBED,
                    payload(characteristic));
        }

        @Override
        public void onNotificationSent(BluetoothDevice device, int status) {
            synchronized (BlePeripheral.this) {
                Notification sent = sending;
                if (sent == null || !sent.device.getAddress().equals(device.getAddress())) return;
                sending = null;
                sent.ticket.done(status == BluetoothGatt.GATT_SUCCESS);
                pump();
            }
        }
    }

    /** {@code value} from {@code offset}, at most {@code limit} bytes of it. */
    static byte[] slice(byte[] value, int offset, int limit) {
        int length = Math.max(0, Math.min(value.length - offset, limit));
        return Arrays.copyOfRange(value, offset, offset + length);
    }
}
