package com.migo.runtime.internal.platform;

/**
 * Version-independent Bluetooth permission policy.
 *
 * <p>This class deliberately has no Android dependencies so the API-level
 * boundary can be verified with host-JVM tests.
 */
final class BluetoothPermissionPolicy {
    private static final int ANDROID_12_API = 31;

    private BluetoothPermissionPolicy() {}

    static boolean canConnect(int sdkInt, boolean bluetoothConnectGranted) {
        return sdkInt < ANDROID_12_API || bluetoothConnectGranted;
    }

    static boolean canAdvertise(int sdkInt, boolean bluetoothAdvertiseGranted) {
        return sdkInt < ANDROID_12_API || bluetoothAdvertiseGranted;
    }

    static boolean canScan(
            int sdkInt,
            boolean bluetoothScanGranted,
            boolean fineLocationGranted) {
        return sdkInt >= ANDROID_12_API
                ? bluetoothScanGranted
                : fineLocationGranted;
    }
}
