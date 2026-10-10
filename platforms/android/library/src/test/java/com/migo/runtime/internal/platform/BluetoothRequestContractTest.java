package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.json.JSONObject;
import org.junit.Test;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.Iterator;
import java.util.List;
import java.util.Locale;

/**
 * A Bluetooth answer reaches the request that asked for it.
 *
 * <p>The runtime routes an answer by the method number it carries, one table for
 * every SDK. This SDK's numbers are {@link BluetoothManager.Method}'s ordinals; a
 * reordering would deliver, say, a write's answer to a pending read -- silently,
 * since both are valid answers. So the order is checked against the contract the
 * runtime's table is checked against.
 */
public final class BluetoothRequestContractTest {

    private static final String CONTRACT = "contracts/runtime/host-services.json";

    @Test
    public void every_method_number_is_the_contracts() throws Exception {
        JSONObject methods = new JSONObject(new String(
                Files.readAllBytes(locateContract()), StandardCharsets.UTF_8))
                .getJSONObject("services").getJSONObject("bluetooth").getJSONObject("methods");
        List<String> declared = new ArrayList<>();
        for (Iterator<String> names = methods.keys(); names.hasNext(); ) {
            declared.add(names.next());
        }
        assertEquals("one Method per contract method",
                declared.size(), BluetoothManager.Method.values().length);
        for (String name : declared) {
            BluetoothManager.Method method =
                    BluetoothManager.Method.valueOf(name.toUpperCase(Locale.ROOT));
            assertEquals(name, methods.getJSONObject(name).getInt("id"), method.ordinal());
        }
    }

    @Test
    public void a_request_is_answered_once_with_its_id_and_code() throws Exception {
        List<String> delivered = new ArrayList<>();
        List<Integer> methods = new ArrayList<>();
        BluetoothManager.Reply reply = new BluetoothManager.Reply(
                9, BluetoothManager.Method.CREATE_BLE_CONNECTION, 41,
                (session, method, json) -> {
                    methods.add(method);
                    delivered.add(json);
                });

        reply.fail(BluetoothManager.OPERATE_TIME_OUT, "timeout");
        reply.ok();

        assertEquals(1, delivered.size());
        assertTrue(reply.isAnswered());
        assertEquals(BluetoothManager.Method.CREATE_BLE_CONNECTION.ordinal(), (int) methods.get(0));
        JSONObject answer = new JSONObject(delivered.get(0));
        assertEquals(41, answer.getInt("requestId"));
        assertEquals(10012, answer.getInt("errCode"));
        assertEquals("createBLEConnection:fail timeout", answer.getString("error"));
    }

    @Test
    public void a_success_carries_its_result_and_no_error() throws Exception {
        List<String> delivered = new ArrayList<>();
        BluetoothManager.Reply reply = new BluetoothManager.Reply(
                9, BluetoothManager.Method.GET_BLE_DEVICE_RSSI, 7,
                (session, method, json) -> delivered.add(json));

        reply.ok(new JSONObject().put("RSSI", -61));

        JSONObject answer = new JSONObject(delivered.get(0));
        assertEquals(-61, answer.getInt("RSSI"));
        assertEquals(7, answer.getInt("requestId"));
        assertFalse(answer.has("error"));
    }

    private static Path locateContract() {
        StringBuilder searched = new StringBuilder();
        Path directory = Paths.get("").toAbsolutePath();
        while (directory != null) {
            Path candidate = directory.resolve(CONTRACT);
            if (Files.isReadable(candidate)) {
                return candidate;
            }
            searched.append("\n  ").append(candidate);
            directory = directory.getParent();
        }
        throw new AssertionError("host-service contract not found, searched:" + searched);
    }
}
