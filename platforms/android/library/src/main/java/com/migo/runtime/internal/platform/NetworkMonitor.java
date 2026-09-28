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
import android.text.TextUtils;

import com.migo.runtime.internal.NativeMethods;

import java.net.Inet4Address;
import java.net.InetAddress;
import java.net.InterfaceAddress;
import java.net.NetworkInterface;
import java.util.Enumeration;
import java.util.List;

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

                    // Calculate signal strength and weak net status
                    if ("wifi".equals(networkType)) {
                        signalStrength = getWifiSignalStrength();
                    } else {
                        signalStrength = getCellularSignalStrength();
                    }
                }
            }

            // Simple heuristic for weak network based on signal strength
            // This is a simplified logic, real implementation might be more complex
            if (isConnected && "wifi".equals(networkType) && signalStrength < -85) {
                weakNet = true;
            }

            return new NetworkStatus(isConnected, networkType, signalStrength, hasSystemProxy, weakNet, null);

        } catch (SecurityException e) {
            return new NetworkStatus(false, "none", "getNetworkType:fail:no permission (android.permission.ACCESS_NETWORK_STATE)");
        } catch (Exception e) {
            return new NetworkStatus(false, "none", "getNetworkType:fail:" + e.getMessage());
        }
    }

    private int getCellularSignalStrength() {
        try {
            TelephonyManager tm = (TelephonyManager) context.getSystemService(Context.TELEPHONY_SERVICE);
            if (tm == null) return 0;
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                android.telephony.SignalStrength ss = tm.getSignalStrength();
                if (ss != null) {
                    // getLevel() returns 0-4; map to approximate dBm range
                    int level = ss.getLevel();
                    // Map: 0 → -120, 1 → -105, 2 → -90, 3 → -75, 4 → -60
                    return -120 + level * 15;
                }
            }
            // Fallback: no direct API below P without READ_PHONE_STATE permission
            return 0;
        } catch (Exception e) {
            return 0;
        }
    }

    private int getWifiSignalStrength() {
        try {
            WifiManager wifiManager = (WifiManager) context.getApplicationContext().getSystemService(Context.WIFI_SERVICE);
            WifiInfo wifiInfo = wifiManager.getConnectionInfo();
            if (wifiInfo != null) {
                return wifiInfo.getRssi();
            }
        } catch (Exception e) {
            android.util.Log.e("NetworkMonitor", "getWifiSignalStrength failed", e);
        }
        return 0;
    }

    private boolean checkSystemProxy() {
        try {
            String host = System.getProperty("http.proxyHost");
            String port = System.getProperty("http.proxyPort");
            return !TextUtils.isEmpty(host) && !TextUtils.isEmpty(port);
        } catch (Exception e) {
            return false;
        }
    }

    /**
     * Get network type string from NetworkCapabilities (available from API 23; this SDK requires API 26+).
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
     * Get local IP address and netmask.
     *
     * @return LocalIPInfo with localip and netmask
     */
    public static LocalIPInfo getLocalIPAddress() {
        try {
            Enumeration<NetworkInterface> interfaces = NetworkInterface.getNetworkInterfaces();
            while (interfaces.hasMoreElements()) {
                NetworkInterface networkInterface = interfaces.nextElement();
                if (networkInterface.isLoopback() || !networkInterface.isUp()) {
                    continue;
                }

                List<InterfaceAddress> ifAddresses = networkInterface.getInterfaceAddresses();
                for (InterfaceAddress ifAddr : ifAddresses) {
                    InetAddress address = ifAddr.getAddress();
                    if (address instanceof Inet4Address && !address.isLoopbackAddress()) {
                        String localip = address.getHostAddress();
                        // Get netmask from the same InterfaceAddress that matched
                        short prefixLength = ifAddr.getNetworkPrefixLength();
                        String netmask = prefixToNetmask(prefixLength);
                        return new LocalIPInfo(localip, netmask, null);
                    }
                }
            }
        } catch (SecurityException e) {
            return new LocalIPInfo("", "", "getLocalIPAddress:fail:no permission (INTERNET)");
        } catch (Exception e) {
            return new LocalIPInfo("", "", "getLocalIPAddress:fail:" + e.getMessage());
        }
        return new LocalIPInfo("", "", null);
    }

    /**
     * Convert prefix length to netmask string.
     */
    private static String prefixToNetmask(short prefixLength) {
        // Java shift counts are taken mod 32, so `0xFFFFFFFF << 32` is a
        // no-op (a /0 prefix would wrongly yield 255.255.255.255 instead
        // of 0.0.0.0); special-case the zero prefix.
        int mask = prefixLength <= 0 ? 0 : (0xFFFFFFFF << (32 - prefixLength));
        return String.format("%d.%d.%d.%d",
                (mask >> 24) & 0xFF,
                (mask >> 16) & 0xFF,
                (mask >> 8) & 0xFF,
                mask & 0xFF);
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

        // Legacy overload retained for compatibility; current callers use the full constructor.
        public NetworkStatus(boolean isConnected, String networkType, String error) {
            this(isConnected, networkType, 0, false, false, error);
        }
    }

    /**
     * Local IP info data class.
     */
    public static class LocalIPInfo {
        public final String localip;
        public final String netmask;
        public final String error;

        public LocalIPInfo(String localip, String netmask, String error) {
            this.localip = localip;
            this.netmask = netmask;
            this.error = error;
        }
    }
}
