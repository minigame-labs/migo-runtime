package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import android.bluetooth.BluetoothAdapter;
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

import org.json.JSONObject;
import org.junit.Before;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.annotation.Config;

import java.util.ArrayList;
import java.util.List;
import java.util.UUID;
import java.util.function.Consumer;

/**
 * This device as a peripheral, as a central and content see it: services served
 * one at a time, requests reaching content with a callbackId and answered by it
 * -- or by nothing, as the reference says -- what content never sees answered
 * here, notifications sent one at a time, and the name the device advertised
 * under given back.
 */
@RunWith(RobolectricTestRunner.class)
@Config(sdk = 35)
public final class BlePeripheralTest {

    private static final int SESSION = 3;
    private static final int SERVER = 41;
    private static final UUID SERVICE = UUID.fromString("0000ffe0-0000-1000-8000-00805f9b34fb");
    private static final UUID CHARACTERISTIC = UUID.fromString("0000ffe1-0000-1000-8000-00805f9b34fb");

    /** What the platform server was asked. */
    private static final class FakeGatt implements BlePeripheral.Gatt {
        final List<BluetoothGattService> added = new ArrayList<>();
        final List<String> responses = new ArrayList<>();
        final List<String> notifications = new ArrayList<>();
        boolean closed;

        @Override public boolean addService(BluetoothGattService service) {
            added.add(service);
            return true;
        }

        @Override public boolean removeService(BluetoothGattService service) {
            return added.remove(service);
        }

        @Override public boolean sendResponse(BluetoothDevice device, int requestId, int status, int offset,
                                              byte[] value) {
            responses.add(requestId + " " + status + " " + offset + " " + BluetoothManager.bytesToHex(value));
            return true;
        }

        @Override public boolean sendNotification(BluetoothDevice device, BluetoothGattCharacteristic c,
                                                  boolean confirm, byte[] value) {
            notifications.add(device.getAddress() + " " + confirm + " " + BluetoothManager.bytesToHex(value));
            return true;
        }

        @Override public void close() {
            closed = true;
        }
    }

    /** The radio: one server, an advertiser that starts when told, a name. */
    private static final class FakeRadio implements BlePeripheral.Radio {
        final FakeGatt gatt = new FakeGatt();
        BluetoothGattServerCallback callback;
        final List<AdvertiseCallback> advertising = new ArrayList<>();
        AdvertiseData scanResponse;
        String name = "Phone";
        final List<String> renames = new ArrayList<>();

        @Override public BlePeripheral.Gatt openServer(BluetoothGattServerCallback callback) {
            this.callback = callback;
            return gatt;
        }

        @Override public boolean advertisingSupported() {
            return true;
        }

        @Override public boolean advertisingPermitted() {
            return true;
        }

        @Override public void startAdvertising(AdvertiseSettings settings, AdvertiseData data,
                                               AdvertiseData scanResponse, AdvertiseCallback callback) {
            this.scanResponse = scanResponse;
            advertising.add(callback);
        }

        @Override public void stopAdvertising(AdvertiseCallback callback) {
            advertising.remove(callback);
        }

        @Override public String name() {
            return name;
        }

        @Override public void rename(String name, Consumer<Boolean> done) {
            this.name = name;
            renames.add(name);
            done.accept(true);
        }
    }

    private final List<String> answers = new ArrayList<>();
    private final List<String> events = new ArrayList<>();
    private FakeRadio radio;
    private BlePeripheral peripheral;
    private BluetoothDevice central;
    private BluetoothDevice other;
    private int requests;

    @Before
    public void setUp() throws Exception {
        radio = new FakeRadio();
        peripheral = new BlePeripheral(SESSION, radio,
                (session, event, json) -> events.add(event + " " + json));
        BluetoothAdapter adapter = BluetoothAdapter.getDefaultAdapter();
        central = adapter.getRemoteDevice("AA:BB:CC:DD:EE:01");
        other = adapter.getRemoteDevice("AA:BB:CC:DD:EE:02");
        peripheral.createServer(request("{}"), reply(BlePeripheral.Method.CREATE_SERVER));
    }

    private BluetoothManager.Reply reply(BlePeripheral.Method method) {
        int id = ++requests;
        return new BluetoothManager.Reply(SESSION, method.ordinal(), method.apiName, id,
                (session, number, json) -> answers.add(method.apiName + " " + json));
    }

    private static JSONObject request(String json) throws Exception {
        return new JSONObject(json).put("serverId", SERVER);
    }

    /** One readable, writable, notifying characteristic holding 01 02 03 04 05. */
    private void addService() throws Exception {
        peripheral.addService(request("{\"service\": {\"uuid\": \"FFE0\", \"characteristics\": [{\"uuid\": \"FFE1\","
                + " \"properties\": {\"read\": true, \"write\": true, \"notify\": true, \"indicate\": true},"
                + " \"permission\": {\"readable\": true, \"writeable\": true}, \"value\": \"0102030405\","
                + " \"descriptors\": [{\"uuid\": \"2901\", \"permission\": {\"read\": true}, \"value\": \"6869\"}]}]}}"),
                reply(BlePeripheral.Method.ADD_SERVICE));
        radio.callback.onServiceAdded(BluetoothGatt.GATT_SUCCESS, radio.gatt.added.get(0));
    }

    private BluetoothGattCharacteristic characteristic() {
        return radio.gatt.added.get(0).getCharacteristic(CHARACTERISTIC);
    }

    private void connect(BluetoothDevice device) {
        radio.callback.onConnectionStateChange(device, BluetoothGatt.GATT_SUCCESS, BluetoothProfile.STATE_CONNECTED);
    }

    private void subscribe(BluetoothDevice device, int bits) {
        BluetoothGattDescriptor cccd = characteristic().getDescriptor(BlePeripheral.CCCD);
        radio.callback.onDescriptorWriteRequest(device, 90, cccd, false, true, 0, new byte[] {(byte) bits, 0});
    }

    @Test
    public void services_are_served_one_at_a_time_and_answered_once_served() throws Exception {
        peripheral.addService(request("{\"service\": {\"uuid\": \"FFE0\", \"characteristics\": []}}"),
                reply(BlePeripheral.Method.ADD_SERVICE));
        peripheral.addService(request("{\"service\": {\"uuid\": \"FFF0\", \"characteristics\": []}}"),
                reply(BlePeripheral.Method.ADD_SERVICE));
        assertEquals("the second waits for the first", 1, radio.gatt.added.size());
        assertEquals(1, answers.size());

        radio.callback.onServiceAdded(BluetoothGatt.GATT_SUCCESS, radio.gatt.added.get(0));
        assertEquals(2, radio.gatt.added.size());
        assertTrue(answers.get(1), answers.get(1).startsWith("BLEPeripheralServer.addService {\"requestId\":2"));
        radio.callback.onServiceAdded(BluetoothGatt.GATT_FAILURE, radio.gatt.added.get(1));
        assertTrue(answers.get(2), answers.get(2).contains("service not added"));
    }

    @Test
    public void a_characteristic_that_notifies_gets_the_descriptor_centrals_subscribe_through() throws Exception {
        addService();
        BluetoothGattCharacteristic characteristic = characteristic();
        assertEquals(BluetoothGattCharacteristic.PROPERTY_READ | BluetoothGattCharacteristic.PROPERTY_WRITE
                        | BluetoothGattCharacteristic.PROPERTY_NOTIFY | BluetoothGattCharacteristic.PROPERTY_INDICATE,
                characteristic.getProperties());
        assertEquals(2, characteristic.getDescriptors().size());
        assertTrue(characteristic.getDescriptor(BlePeripheral.CCCD) != null);
    }

    @Test
    public void a_read_reaches_content_and_is_answered_by_its_callback_id() throws Exception {
        addService();
        connect(central);
        radio.callback.onCharacteristicReadRequest(central, 7, 0, characteristic());
        String read = events.get(events.size() - 1);
        assertTrue(read, read.startsWith(BlePeripheral.EVENT_CHARACTERISTIC_READ_REQUEST + " "));
        JSONObject payload = new JSONObject(read.substring(2));
        assertEquals(SERVER, payload.getInt("serverId"));
        assertEquals("FFE0", payload.getString("serviceId"));
        assertEquals("FFE1", payload.getString("characteristicId"));
        assertTrue("content has not answered", radio.gatt.responses.isEmpty());

        // 30 bytes against the default MTU: the answer carries 22, the rest of
        // the long read is served from the value content gave.
        StringBuilder hex = new StringBuilder();
        for (int i = 0; i < 30; i++) hex.append(String.format("%02x", i));
        peripheral.writeCharacteristicValue(request("{\"serviceId\": \"FFE0\", \"characteristicId\": \"FFE1\","
                        + " \"value\": \"" + hex + "\", \"needNotify\": false, \"callbackId\": "
                        + payload.getInt("callbackId") + "}"),
                reply(BlePeripheral.Method.WRITE_CHARACTERISTIC_VALUE));
        assertEquals("7 0 0 " + hex.substring(0, 44), radio.gatt.responses.get(0));
        radio.callback.onCharacteristicReadRequest(central, 8, 22, characteristic());
        assertEquals("8 0 22 " + hex.substring(44), radio.gatt.responses.get(1));
        radio.callback.onCharacteristicReadRequest(central, 9, 31, characteristic());
        assertEquals("9 " + BluetoothGatt.GATT_INVALID_OFFSET + " 31 ", radio.gatt.responses.get(2));

        // Answering again answers nothing: no request waits.
        peripheral.writeCharacteristicValue(request("{\"serviceId\": \"FFE0\", \"characteristicId\": \"FFE1\","
                        + " \"value\": \"00\", \"needNotify\": false, \"callbackId\": "
                        + payload.getInt("callbackId") + "}"),
                reply(BlePeripheral.Method.WRITE_CHARACTERISTIC_VALUE));
        assertEquals(3, radio.gatt.responses.size());
    }

    @Test
    public void a_long_write_reaches_content_whole_once_executed() throws Exception {
        addService();
        connect(central);
        radio.callback.onCharacteristicWriteRequest(central, 10, characteristic(), true, true, 0,
                new byte[] {1, 2});
        radio.callback.onCharacteristicWriteRequest(central, 11, characteristic(), true, true, 2,
                new byte[] {3});
        assertEquals("parts are answered here, not reported", 2, radio.gatt.responses.size());
        int before = events.size();
        radio.callback.onExecuteWrite(central, 12, true);
        assertEquals(before + 1, events.size());
        JSONObject write = new JSONObject(events.get(before).substring(2));
        assertEquals("010203", write.getString("value"));
        assertEquals("12 0 0 ", radio.gatt.responses.get(2));

        // A write a central waits on is answered by content's callbackId.
        radio.callback.onCharacteristicWriteRequest(central, 13, characteristic(), false, true, 0,
                new byte[] {9});
        JSONObject plain = new JSONObject(events.get(events.size() - 1).substring(2));
        assertEquals("09", plain.getString("value"));
        assertEquals(3, radio.gatt.responses.size());
        peripheral.writeCharacteristicValue(request("{\"serviceId\": \"FFE0\", \"characteristicId\": \"FFE1\","
                        + " \"value\": \"09\", \"needNotify\": false, \"callbackId\": "
                        + plain.getInt("callbackId") + "}"),
                reply(BlePeripheral.Method.WRITE_CHARACTERISTIC_VALUE));
        assertEquals("13 0 0 09", radio.gatt.responses.get(3));
    }

    @Test
    public void notifications_go_one_at_a_time_and_the_call_settles_once_all_are_sent() throws Exception {
        addService();
        connect(central);
        connect(other);
        subscribe(central, 0x01);
        subscribe(other, 0x02);
        subscribe(other, 0x02);
        long subscribed = events.stream().filter(e -> e.startsWith(
                BlePeripheral.EVENT_CHARACTERISTIC_SUBSCRIBED + " ")).count();
        assertEquals("a subscription repeated is reported once", 2, subscribed);

        answers.clear();
        peripheral.writeCharacteristicValue(request("{\"serviceId\": \"FFE0\", \"characteristicId\": \"FFE1\","
                + " \"value\": \"aabb\", \"needNotify\": true}"), reply(BlePeripheral.Method.WRITE_CHARACTERISTIC_VALUE));
        assertEquals("one in flight", 1, radio.gatt.notifications.size());
        assertTrue(answers.isEmpty());
        radio.callback.onNotificationSent(central, BluetoothGatt.GATT_SUCCESS);
        radio.callback.onNotificationSent(other, BluetoothGatt.GATT_SUCCESS);
        assertEquals(2, radio.gatt.notifications.size());
        assertTrue(radio.gatt.notifications.contains("AA:BB:CC:DD:EE:01 false aabb"));
        assertTrue("an indication waits on the central's confirmation",
                radio.gatt.notifications.contains("AA:BB:CC:DD:EE:02 true aabb"));
        assertEquals(1, answers.size());
        assertFalse(answers.get(0), answers.get(0).contains("error"));

        // A central that leaves mid-way is not content's failure.
        answers.clear();
        peripheral.writeCharacteristicValue(request("{\"serviceId\": \"FFE0\", \"characteristicId\": \"FFE1\","
                + " \"value\": \"cc\", \"needNotify\": true}"), reply(BlePeripheral.Method.WRITE_CHARACTERISTIC_VALUE));
        radio.callback.onConnectionStateChange(central, BluetoothGatt.GATT_SUCCESS,
                BluetoothProfile.STATE_DISCONNECTED);
        radio.callback.onNotificationSent(other, BluetoothGatt.GATT_SUCCESS);
        assertEquals(1, answers.size());
        assertFalse(answers.get(0), answers.get(0).contains("error"));

        // Unsubscribed: nothing is sent and the call settles at once.
        subscribe(other, 0x00);
        answers.clear();
        int sent = radio.gatt.notifications.size();
        peripheral.writeCharacteristicValue(request("{\"serviceId\": \"FFE0\", \"characteristicId\": \"FFE1\","
                + " \"value\": \"dd\", \"needNotify\": true}"), reply(BlePeripheral.Method.WRITE_CHARACTERISTIC_VALUE));
        assertEquals(sent, radio.gatt.notifications.size());
        assertEquals(1, answers.size());
        assertTrue(events.get(events.size() - 1).startsWith(BlePeripheral.EVENT_CHARACTERISTIC_UNSUBSCRIBED + " "));
    }

    @Test
    public void connections_are_reported_once_each_way() throws Exception {
        connect(central);
        connect(central);
        radio.callback.onConnectionStateChange(central, BluetoothGatt.GATT_SUCCESS,
                BluetoothProfile.STATE_DISCONNECTED);
        assertEquals(2, events.size());
        JSONObject connected = new JSONObject(events.get(0).substring(2));
        assertEquals("AA:BB:CC:DD:EE:01", connected.getString("deviceId"));
        assertTrue(connected.getBoolean("connected"));
        assertFalse(new JSONObject(events.get(1).substring(2)).getBoolean("connected"));
    }

    @Test
    public void a_device_name_is_lent_while_advertised_and_given_back() throws Exception {
        peripheral.startAdvertising(request("{\"advertiseRequest\": {\"deviceName\": \"migo\","
                + " \"serviceUuids\": [\"FFE0\"]}, \"powerLevel\": \"high\"}"),
                reply(BlePeripheral.Method.START_ADVERTISING));
        assertEquals("migo", radio.name);
        assertTrue("the name goes in the scan response", radio.scanResponse.getIncludeDeviceName());
        radio.advertising.get(0).onStartSuccess(null);
        assertTrue(answers.get(answers.size() - 1).startsWith("BLEPeripheralServer.startAdvertising {\"requestId\""));

        peripheral.stopAdvertising(request("{}"), reply(BlePeripheral.Method.STOP_ADVERTISING));
        assertTrue(radio.advertising.isEmpty());
        assertEquals("Phone", radio.name);
    }

    @Test
    public void advertising_that_fails_gives_the_name_back_and_says_why() throws Exception {
        peripheral.startAdvertising(request("{\"advertiseRequest\": {\"deviceName\": \"migo\"}}"),
                reply(BlePeripheral.Method.START_ADVERTISING));
        radio.advertising.get(0).onStartFailure(AdvertiseCallback.ADVERTISE_FAILED_DATA_TOO_LARGE);
        assertTrue(answers.get(answers.size() - 1).contains("advertising data too large"));
        assertEquals("Phone", radio.name);
    }

    @Test
    public void a_later_start_replaces_the_one_before() throws Exception {
        peripheral.startAdvertising(request("{\"advertiseRequest\": {}}"),
                reply(BlePeripheral.Method.START_ADVERTISING));
        peripheral.startAdvertising(request("{\"advertiseRequest\": {\"connectable\": false}}"),
                reply(BlePeripheral.Method.START_ADVERTISING));
        assertEquals(1, radio.advertising.size());
        assertTrue(answers.get(answers.size() - 1).contains("replaced by a later startAdvertising"));
        assertNull("no name, no scan response", radio.scanResponse);
    }

    @Test
    public void closing_fails_what_waits_and_closes_the_server() throws Exception {
        peripheral.addService(request("{\"service\": {\"uuid\": \"FFE0\", \"characteristics\": []}}"),
                reply(BlePeripheral.Method.ADD_SERVICE));
        peripheral.startAdvertising(request("{\"advertiseRequest\": {\"deviceName\": \"migo\"}}"),
                reply(BlePeripheral.Method.START_ADVERTISING));
        peripheral.close();
        assertTrue(radio.gatt.closed);
        assertTrue(radio.advertising.isEmpty());
        assertEquals("Phone", radio.name);
        assertTrue(answers.stream().anyMatch(a -> a.startsWith("BLEPeripheralServer.addService")
                && a.contains("adapter closed")));
    }

    @Test
    public void an_ibeacon_is_laid_out_as_the_format_has_it() throws Exception {
        byte[] beacon = BlePeripheral.iBeacon(new JSONObject("{\"uuid\": \"E2C56DB5-DFFB-48D2-B060-D0F5A71096E0\","
                + " \"major\": 258, \"minor\": 772, \"measuredPower\": -59}"));
        assertEquals("0215e2c56db5dffb48d2b060d0f5a71096e001020304c5", BluetoothManager.bytesToHex(beacon));
    }

    @Test
    public void uuids_are_read_as_content_names_them() {
        assertEquals(SERVICE, BlePeripheral.uuid("FFE0"));
        assertEquals(UUID.fromString("1234ffe0-0000-1000-8000-00805f9b34fb"), BlePeripheral.uuid("1234FFE0"));
        assertEquals(CHARACTERISTIC, BlePeripheral.uuid("0000FFE1-0000-1000-8000-00805F9B34FB"));
        try {
            BlePeripheral.uuid("FFE");
            throw new AssertionError("a UUID of 12 bits");
        } catch (BluetoothManager.BluetoothFailure expected) {
            assertEquals(BluetoothManager.INVALID_DATA, expected.errCode);
        }
        assertArrayEquals(new byte[] {2, 3}, BlePeripheral.slice(new byte[] {1, 2, 3, 4}, 1, 2));
    }
}
