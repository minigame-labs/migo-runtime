package com.migo.runtime.internal;

import android.app.Activity;

import com.migo.runtime.internal.platform.NetworkMonitor;

import org.json.JSONException;
import org.json.JSONObject;

import java.util.concurrent.ConcurrentHashMap;

/**
 * Domain class for per-session network monitor delegation.
 *
 * @hide
 */
public final class NetworkExports {

    private NetworkExports() {}

    private static final Object sNetworkLock = new Object();

    private static final ConcurrentHashMap<Integer, NetworkMonitor> sNetworkMonitors =
            new ConcurrentHashMap<>();

    private static NetworkMonitor getOrCreateNetworkMonitor(int sessionId) {
        NetworkMonitor existing = sNetworkMonitors.get(sessionId);
        if (existing != null) return existing;
        synchronized (sNetworkLock) {
            existing = sNetworkMonitors.get(sessionId);
            if (existing != null) return existing;
            RuntimeContext ctx = RuntimeRegistry.get(sessionId);
            if (ctx == null) return null;
            Activity activity = ctx.getActivity();
            if (activity == null) return null;
            NetworkMonitor mgr = new NetworkMonitor(sessionId, activity);
            sNetworkMonitors.put(sessionId, mgr);
            return mgr;
        }
    }

    public static void startNetworkMonitoring(int sessionId) {
        NetworkMonitor mgr = getOrCreateNetworkMonitor(sessionId);
        if (mgr != null) {
            mgr.startMonitoring();
        }
    }

    public static void stopNetworkMonitoring(int sessionId) {
        NetworkMonitor mgr = sNetworkMonitors.get(sessionId);
        if (mgr != null) {
            mgr.stopMonitoring();
        }
    }

    public static String getNetworkTypeJson(int sessionId) {
        NetworkMonitor mgr = getOrCreateNetworkMonitor(sessionId);
        if (mgr == null) {
            return "{\"networkType\":\"none\",\"isConnected\":false,\"signalStrength\":0,\"hasSystemProxy\":false,\"weakNet\":false}";
        }

        NetworkMonitor.NetworkStatus status = mgr.getNetworkStatus();
        if (status.error != null) {
            try {
                JSONObject err = new JSONObject();
                JSONObject inner = new JSONObject();
                inner.put("errMsg", status.error);
                err.put("_error", inner);
                return err.toString();
            } catch (JSONException e) {
                return "{}";
            }
        }

        try {
            JSONObject json = new JSONObject();
            json.put("networkType", status.networkType);
            json.put("isConnected", status.isConnected);
            json.put("signalStrength", status.signalStrength);
            json.put("hasSystemProxy", status.hasSystemProxy);
            json.put("weakNet", status.weakNet);
            return json.toString();
        } catch (JSONException e) {
            return "{}";
        }
    }

    public static void destroyNetworkMonitor(int sessionId) {
        ResourceCleanup.destroyMatching(
                sNetworkMonitors,
                id -> id == sessionId,
                NetworkMonitor::destroy);
    }

    public static void destroyAll(int sessionId) {
        destroyNetworkMonitor(sessionId);
    }
}
