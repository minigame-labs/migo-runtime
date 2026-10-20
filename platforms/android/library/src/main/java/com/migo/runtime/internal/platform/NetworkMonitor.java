package com.migo.runtime.internal.platform;

import android.content.Context;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkRequest;
import android.net.wifi.WifiInfo;
import android.net.wifi.WifiManager;
import android.os.Build;
import android.telephony.TelephonyManager;

import com.migo.runtime.internal.NativeMethods;

/**
 * Network status monitoring utility.
 * <p>
 * Uses ConnectivityManager and NetworkCallback to monitor network changes.
 * Compatible with Android API 26+.
 *
 * @hide
 */
public final class NetworkMonitor {

    private final int sessionId;
    private final Context context;
    private final ConnectivityManager connectivityManager;
    private ConnectivityManager.NetworkCallback networkCallback;

    public NetworkMonitor(int sessionId, Context context) {
        this.sessionId = sessionId;
        this.context = context;
        this.connectivityManager = (ConnectivityManager) context.getSystemService(Context.CONNECTIVITY_SERVICE);
    }

    /**
     * Start monitoring network status changes.
     */
    public void startMonitoring() {
        if (connectivityManager == null) return;
        stopMonitoring();

        networkCallback = new ConnectivityManager.NetworkCallback() {
            @Override
            public void onAvailable(Network network) {
                notifyNetworkChange();
            }

            @Override
            public void onLost(Network network) {
                notifyNetworkChange();
            }

            @Override
            public void onCapabilitiesChanged(Network network, NetworkCapabilities capabilities) {
                notifyNetworkChange();
            }
        };

        NetworkRequest request = new NetworkRequest.Builder()
                .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                .build();

        try {
            connectivityManager.registerNetworkCallback(request, networkCallback);
        } catch (Exception e) {
            // SecurityException if missing ACCESS_NETWORK_STATE permission
        }
    }

    /**
     * Stop monitoring network status changes.
     */
    public void stopMonitoring() {
        if (connectivityManager != null && networkCallback != null) {
            try {
                connectivityManager.unregisterNetworkCallback(networkCallback);
            } catch (Exception ignored) {
            }
            networkCallback = null;
        }
    }

    /**
     * Notify JS layer of network status change.
     */
    private void notifyNetworkChange() {
        NetworkStatus status = getNetworkStatus();
        NativeMethods.onNetworkStatusChange(sessionId, status.isConnected, status.networkType);
    }

    /**
     * Get current network status.
     */
    public NetworkStatus getNetworkStatus() {
        if (connectivityManager == null) {
            return new NetworkStatus(false, "none", "ConnectivityManager is null");
        }

        try {
            String networkType = "none";
            boolean isConnected = false;
            int signalStrength = 0;
            boolean hasSystemProxy = checkSystemProxy();
            boolean weakNet = false;

            Network network = connectivityManager.getActiveNetwork();
            if (network != null) {
                NetworkCapabilities capabilities = connectivityManager.getNetworkCapabilities(network);
                if (capabilities != null) {
                    networkType = getNetworkTypeFromCapabilities(capabilities);
                    isConnected = capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                            && capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED);

                    // The received signal in dBm, 0 where the platform does not say.
                    if (capabilities.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) {
                        signalStrength = getWifiSignalStrength(capabilities);
                    } else if (capabilities.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)) {
                        signalStrength = getCellularSignalStrength();
                    }
                }
            }

            // The SDK's judgement of a weak network: a Wi-Fi signal below -85 dBm, where
            // throughput falls off on most access points. A signal it could not read is
            // not judged weak.
            if (isConnected && "wifi".equals(networkType) && signalStrength != 0 && signalStrength < -85) {
                weakNet = true;
            }

            return new NetworkStatus(isConnected, networkType, signalStrength, hasSystemProxy, weakNet, null);

        } catch (SecurityException e) {
            return new NetworkStatus(false, "none", "getNetworkType:fail:no permission (android.permission.ACCESS_NETWORK_STATE)");
        } catch (Exception e) {
            return new NetworkStatus(false, "none", "getNetworkType:fail:" + e.getMessage());
        }
    }

    /**
     * The signal of the cell the device is camped on, in dBm, or 0 where the platform
     * does not say. Per-cell strengths (API 29) carry a real dBm; a level of 0-4 is all
     * that is readable before them, and it is not mapped to a made-up dBm.
     */
    private int getCellularSignalStrength() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return 0;
        try {
            TelephonyManager tm = (TelephonyManager) context.getSystemService(Context.TELEPHONY_SERVICE);
            android.telephony.SignalStrength ss = tm != null ? tm.getSignalStrength() : null;
            if (ss == null) return 0;
            for (android.telephony.CellSignalStrength cell : ss.getCellSignalStrengths()) {
                int dbm = cell.getDbm();
                if (dbm != android.telephony.CellInfo.UNAVAILABLE && dbm < 0) return dbm;
            }
        } catch (RuntimeException e) {
            android.util.Log.w("NetworkMonitor", "the cell's signal could not be read", e);
        }
        return 0;
    }

    /**
     * The Wi-Fi network's RSSI in dBm, or 0 where the platform does not say: from the
     * network's own capabilities (API 29), which need no Wi-Fi permission, and from the
     * Wi-Fi service before them.
     */
    @SuppressWarnings("deprecation")
    private int getWifiSignalStrength(NetworkCapabilities capabilities) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            int rssi = capabilities.getSignalStrength();
            return rssi != NetworkCapabilities.SIGNAL_STRENGTH_UNSPECIFIED && rssi < 0 ? rssi : 0;
        }
        try {
            WifiManager wifiManager = (WifiManager) context.getApplicationContext().getSystemService(Context.WIFI_SERVICE);
            WifiInfo wifiInfo = wifiManager != null ? wifiManager.getConnectionInfo() : null;
            int rssi = wifiInfo != null ? wifiInfo.getRssi() : 0;
            return rssi < 0 ? rssi : 0;
        } catch (RuntimeException e) {
            android.util.Log.w("NetworkMonitor", "the Wi-Fi signal could not be read", e);
            return 0;
        }
    }

    /**
     * Whether the system routes the network through a proxy -- one set by hand or a
     * configuration script -- as getNetworkType's hasSystemProxy says. The JVM's
     * {@code http.proxyHost}, read before, is this process's own setting, not the
     * device's.
     */
    private boolean checkSystemProxy() {
        android.net.ProxyInfo proxy = connectivityManager.getDefaultProxy();
        if (proxy == null) return false;
        String host = proxy.getHost();
        return (host != null && !host.isEmpty())
                || !android.net.Uri.EMPTY.equals(proxy.getPacFileUrl());
    }

    /**
     * Get network type string from NetworkCapabilities (API 23+).
     */
    @SuppressWarnings("deprecation")
    private String getNetworkTypeFromCapabilities(NetworkCapabilities capabilities) {
        if (capabilities.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) {
            return "wifi";
        } else if (capabilities.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)) {
            // Android protects radio-generation details with phone permissions.
            // Connectivity itself does not require that privacy-sensitive grant.
            return "unknown";
        } else if (capabilities.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET)) {
            return "wifi"; // Treat ethernet as wifi for simplicity
        } else {
            return "unknown";
        }
    }

    /**
     * Clean up resources.
     */
    public void destroy() {
        stopMonitoring();
    }

    /**
     * Network status data class.
     */
    public static class NetworkStatus {
        public final boolean isConnected;
        public final String networkType;
        public final int signalStrength;
        public final boolean hasSystemProxy;
        public final boolean weakNet;
        public final String error;

        public NetworkStatus(boolean isConnected, String networkType, int signalStrength, boolean hasSystemProxy, boolean weakNet, String error) {
            this.isConnected = isConnected;
            this.networkType = networkType;
            this.signalStrength = signalStrength;
            this.hasSystemProxy = hasSystemProxy;
            this.weakNet = weakNet;
            this.error = error;
        }

        // Keep old constructor for compatibility if needed or update callers
        public NetworkStatus(boolean isConnected, String networkType, String error) {
            this(isConnected, networkType, 0, false, false, error);
        }
    }
}
