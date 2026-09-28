package com.migo.runtime.internal.platform;

import android.Manifest;
import android.content.Context;
import android.content.pm.PackageManager;
import android.location.Location;
import android.location.LocationListener;
import android.location.LocationManager;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;

import com.migo.runtime.internal.NativeMethods;
import com.migo.runtime.internal.PermissionOperationGate;

import org.json.JSONException;
import com.migo.runtime.internal.CallbackCorrelation;

import org.json.JSONObject;

import java.util.function.Consumer;

/**
 * Async location provider for getLocation / getFuzzyLocation.
 *
 * <p>Design decisions for broad Android compatibility (API 26+, domestic & international):
 * <ul>
 *   <li>Uses {@link LocationManager} directly (not Google Play Services / FusedLocationProvider)
 *       to work on devices without GMS (Huawei, many domestic Chinese phones).</li>
 *   <li>Tries GPS_PROVIDER first for high accuracy, falls back to NETWORK_PROVIDER.</li>
 *   <li>Uses {@link LocationManager#getLastKnownLocation} first for a quick result,
 *       then requests a single async update if high accuracy is needed or
 *       last-known is stale (>60s).</li>
 *   <li>Coordinate conversion (WGS-84 to GCJ-02) is done on the Java side.</li>
 *   <li>Results are delivered via {@link NativeMethods} callbacks — no thread blocking.</li>
 * </ul>
 */
public final class LocationProvider {

    private static final long STALE_THRESHOLD_MS = 60_000; // 60s
    private static final long DEFAULT_TIMEOUT_MS = 10_000; // 10s default timeout
    private static final Handler MAIN_HANDLER = new Handler(Looper.getMainLooper());

    private LocationProvider() {}

    interface RequestRemoval {
        void removeListener();
        void removeTimeout();
    }

    /** Retains cleanup handles and the first result until both removals succeed. */
    static final class RetainedRequest<T> {
        private final RequestRemoval removal;
        private final Runnable finish;
        private final Consumer<T> deliver;
        private final Consumer<RuntimeException> failureReporter;
        private boolean listenerRemoved;
        private boolean timeoutRemoved;
        private boolean resultSet;
        private boolean released;
        private boolean cleanupRunning;
        private long cleanupGeneration;
        private RuntimeException lastCleanupFailure;
        private T result;
        private boolean cancelled;
        private T cancellationResult;

        RetainedRequest(
                RequestRemoval removal,
                Runnable finish,
                Consumer<T> deliver,
                Consumer<RuntimeException> failureReporter) {
            this.removal = removal;
            this.finish = finish;
            this.deliver = deliver;
            this.failureReporter = failureReporter;
        }

        void complete(T candidate) {
            attempt(candidate, false);
        }

        void cancel(T fallback) {
            attempt(fallback, true);
        }

        private void attempt(T candidate, boolean cancellation) {
            boolean removeListener;
            boolean removeTimeout;
            synchronized (this) {
                if (released) return;
                if (!resultSet) {
                    result = candidate;
                    resultSet = true;
                }
                if (cancellation) {
                    cancellationResult = candidate;
                    cancelled = true;
                }
                if (cleanupRunning) {
                    if (!cancellation) return;
                    long observedGeneration = cleanupGeneration;
                    while (cleanupRunning) {
                        try {
                            wait();
                        } catch (InterruptedException interrupted) {
                            Thread.currentThread().interrupt();
                            throw new IllegalStateException(
                                    "interrupted while waiting for location request cleanup",
                                    interrupted);
                        }
                    }
                    if (released) return;
                    if (cleanupGeneration != observedGeneration
                            && lastCleanupFailure != null) {
                        throw lastCleanupFailure;
                    }
                }
                cleanupRunning = true;
                lastCleanupFailure = null;
                removeListener = !listenerRemoved;
                removeTimeout = !timeoutRemoved;
            }

            RuntimeException failure = null;
            boolean listenerRemovalSucceeded = false;
            if (removeListener) {
                try {
                    removal.removeListener();
                    listenerRemovalSucceeded = true;
                } catch (RuntimeException error) {
                    failure = error;
                }
            }
            boolean timeoutRemovalSucceeded = false;
            if (removeTimeout) {
                try {
                    removal.removeTimeout();
                    timeoutRemovalSucceeded = true;
                } catch (RuntimeException error) {
                    if (failure == null) {
                        failure = error;
                    } else {
                        failure.addSuppressed(error);
                    }
                }
            }

            boolean release;
            T deliveryResult;
            synchronized (this) {
                if (listenerRemovalSucceeded) listenerRemoved = true;
                if (timeoutRemovalSucceeded) timeoutRemoved = true;
                cleanupRunning = false;
                cleanupGeneration++;
                lastCleanupFailure = failure;
                release = failure == null && listenerRemoved && timeoutRemoved;
                if (release) released = true;
                deliveryResult = cancelled ? cancellationResult : result;
                notifyAll();
            }

            if (failure != null) {
                try {
                    failureReporter.accept(failure);
                } catch (RuntimeException reportFailure) {
                    failure.addSuppressed(reportFailure);
                    android.util.Log.e("LocationProvider",
                            "location_cleanup_failure_reporting_failed", failure);
                }
                if (cancellation) throw failure;
                return;
            }
            if (release) {
                finish.run();
                deliver.accept(deliveryResult);
            }
        }

        synchronized boolean isReleased() {
            return released;
        }
    }

    // ==================== Public async entry points ====================

    /**
     * Start an async location request (getLocation).
     * Result is delivered via {@link NativeMethods#onLocationResult}.
     *
     * @param context     Android context
     * @param sessionId   Session ID for the callback
     * @param optionsJson JSON with: type, altitude, isHighAccuracy, highAccuracyExpireTime
     */
    public static void getLocationAsync(
            Context context,
            int sessionId,
            String optionsJson,
            PermissionOperationGate gate,
            PermissionOperationGate.Pending pending,
            Consumer<RuntimeException> cleanupFailureReporter) {
        boolean[] async = {false};
        // Read before the parse: a malformed options string must still be
        // answered to the request that sent it, and `requestIdOf` treats
        // unparseable input as "no id" rather than throwing.
        final int requestId = CallbackCorrelation.requestIdOf(optionsJson);
        try {
            JSONObject opts = new JSONObject(optionsJson);
            String type = opts.optString("type", "wgs84");
            boolean altitude = opts.optBoolean("altitude", false);
            boolean isHighAccuracy = opts.optBoolean("isHighAccuracy", false);
            long highAccuracyExpireTime = opts.optLong("highAccuracyExpireTime", 0);

            if (!hasLocationPermission(context)) {
                NativeMethods.onLocationResult(sessionId,
                        errorJson(requestId, "getLocation", "no location permission"));
                return;
            }

            LocationManager lm = (LocationManager) context.getSystemService(Context.LOCATION_SERVICE);
            if (lm == null) {
                NativeMethods.onLocationResult(sessionId,
                        errorJson(requestId, "getLocation", "location service unavailable"));
                return;
            }

            long timeoutMs = DEFAULT_TIMEOUT_MS;
            if (isHighAccuracy && highAccuracyExpireTime > 0) {
                timeoutMs = highAccuracyExpireTime;
            }

            boolean useGps = isHighAccuracy || isProviderEnabled(lm, LocationManager.GPS_PROVIDER);
            boolean useNetwork = isProviderEnabled(lm, LocationManager.NETWORK_PROVIDER);

            if (!useGps && !useNetwork) {
                NativeMethods.onLocationResult(sessionId,
                        errorJson(requestId, "getLocation", "location service disabled"));
                return;
            }

            // Try last known first
            Location lastKnown = getBestLastKnown(lm, useGps, useNetwork);

            boolean needFresh = isHighAccuracy || lastKnown == null || isStale(lastKnown);

            if (!needFresh && lastKnown != null) {
                // Fast path: return immediately
                NativeMethods.onLocationResult(sessionId,
                        buildLocationResult(requestId, lastKnown, type, altitude));
                return;
            }

            // Async path: request a single update with timeout
            requestSingleUpdateAsync(lm, useGps, useNetwork, timeoutMs, lastKnown,
                    gate, pending,
                    cleanupFailureReporter,
                    (location) -> {
                        if (location != null) {
                            NativeMethods.onLocationResult(sessionId,
                                    buildLocationResult(requestId, location, type, altitude));
                        } else {
                            NativeMethods.onLocationResult(sessionId,
                                    errorJson(requestId, "getLocation", "unable to get location"));
                        }
                    });
            async[0] = true;

        } catch (Exception e) {
            NativeMethods.onLocationResult(sessionId,
                    errorJson(requestId, "getLocation", e.getMessage()));
        } finally {
            if (!async[0]) gate.finish(pending);
        }
    }

    /**
     * Start an async fuzzy location request (getFuzzyLocation).
     * Result is delivered via {@link NativeMethods#onFuzzyLocationResult}.
     *
     * @param context     Android context
     * @param sessionId   Session ID for the callback
     * @param optionsJson JSON with: type
     */
    public static void getFuzzyLocationAsync(
            Context context,
            int sessionId,
            String optionsJson,
            PermissionOperationGate gate,
            PermissionOperationGate.Pending pending,
            Consumer<RuntimeException> cleanupFailureReporter) {
        boolean[] async = {false};
        // Read before the parse: a malformed options string must still be
        // answered to the request that sent it, and `requestIdOf` treats
        // unparseable input as "no id" rather than throwing.
        final int requestId = CallbackCorrelation.requestIdOf(optionsJson);
        try {
            JSONObject opts = new JSONObject(optionsJson);
            String type = opts.optString("type", "wgs84");

            if (!hasLocationPermission(context)) {
                NativeMethods.onFuzzyLocationResult(sessionId,
                        errorJson(requestId, "getFuzzyLocation", "no location permission"));
                return;
            }

            LocationManager lm = (LocationManager) context.getSystemService(Context.LOCATION_SERVICE);
            if (lm == null) {
                NativeMethods.onFuzzyLocationResult(sessionId,
                        errorJson(requestId, "getFuzzyLocation", "location service unavailable"));
                return;
            }

            boolean useGps = isProviderEnabled(lm, LocationManager.GPS_PROVIDER);
            boolean useNetwork = isProviderEnabled(lm, LocationManager.NETWORK_PROVIDER);

            if (!useGps && !useNetwork) {
                NativeMethods.onFuzzyLocationResult(sessionId,
                        errorJson(requestId, "getFuzzyLocation", "location service disabled"));
                return;
            }

            // Prefer network for fuzzy (lower accuracy is fine)
            Location lastKnown = null;
            if (useNetwork) {
                lastKnown = getLastKnownSafe(lm, LocationManager.NETWORK_PROVIDER);
            }
            if (lastKnown == null && useGps) {
                lastKnown = getLastKnownSafe(lm, LocationManager.GPS_PROVIDER);
            }

            if (lastKnown != null) {
                // Fast path
                NativeMethods.onFuzzyLocationResult(sessionId,
                        buildFuzzyResult(requestId, lastKnown, type));
                return;
            }

            // Async path
            requestSingleUpdateAsync(lm, false, useNetwork, DEFAULT_TIMEOUT_MS, null,
                    gate, pending,
                    cleanupFailureReporter,
                    (location) -> {
                        if (location != null) {
                            NativeMethods.onFuzzyLocationResult(sessionId,
                                    buildFuzzyResult(requestId, location, type));
                        } else {
                            NativeMethods.onFuzzyLocationResult(sessionId,
                                    errorJson(requestId, "getFuzzyLocation", "unable to get location"));
                        }
                    });
            async[0] = true;

        } catch (Exception e) {
            NativeMethods.onFuzzyLocationResult(sessionId,
                    errorJson(requestId, "getFuzzyLocation", e.getMessage()));
        } finally {
            if (!async[0]) gate.finish(pending);
        }
    }

    // ==================== Async location request ====================

    private interface LocationCallback {
        void onResult(Location location);
    }

    /**
     * Request a single location update asynchronously.
     * The callback is invoked on the main thread when a location arrives or timeout expires.
     * No thread is blocked.
     */
    @SuppressWarnings("MissingPermission")
    private static void requestSingleUpdateAsync(
            LocationManager lm, boolean useGps, boolean useNetwork,
            long timeoutMs, Location fallback,
            PermissionOperationGate gate,
            PermissionOperationGate.Pending pending,
            Consumer<RuntimeException> cleanupFailureReporter,
            LocationCallback callback) {

        final Location[] best = new Location[]{ null };
        final Runnable[] timeout = new Runnable[1];
        @SuppressWarnings("unchecked")
        final RetainedRequest<Location>[] request = new RetainedRequest[1];

        LocationListener listener = new LocationListener() {
            @Override
            public void onLocationChanged(Location location) {
                if (best[0] == null || isBetter(location, best[0])) {
                    best[0] = location;
                }
                request[0].complete(best[0]);
            }

            @Override
            public void onStatusChanged(String provider, int status, Bundle extras) {}

            @Override
            public void onProviderEnabled(String provider) {}

            @Override
            public void onProviderDisabled(String provider) {
                request[0].complete(fallback);
            }
        };

        timeout[0] = () -> request[0].complete(fallback);
        request[0] = new RetainedRequest<>(
                new RequestRemoval() {
                    @Override public void removeListener() {
                        lm.removeUpdates(listener);
                    }

                    @Override public void removeTimeout() {
                        MAIN_HANDLER.removeCallbacks(timeout[0]);
                    }
                },
                () -> gate.finish(pending),
                callback::onResult,
                cleanupFailureReporter);
        pending.setCancellation(() -> request[0].cancel(fallback));

        boolean timeoutPosted;
        try {
            timeoutPosted = MAIN_HANDLER.postDelayed(timeout[0], timeoutMs);
        } catch (RuntimeException postFailure) {
            request[0].complete(fallback);
            return;
        }
        if (!timeoutPosted) {
            request[0].complete(fallback);
            return;
        }

        // Request updates on main thread (requires Looper)
        boolean posted;
        try {
            posted = MAIN_HANDLER.post(() -> {
                try {
                    gate.enter(pending, () -> {
                        if (useGps && isProviderEnabled(lm, LocationManager.GPS_PROVIDER)) {
                            lm.requestSingleUpdate(LocationManager.GPS_PROVIDER, listener,
                                    Looper.getMainLooper());
                        }
                        if (useNetwork && isProviderEnabled(lm, LocationManager.NETWORK_PROVIDER)) {
                            lm.requestSingleUpdate(LocationManager.NETWORK_PROVIDER, listener,
                                    Looper.getMainLooper());
                        }
                    });
                } catch (RuntimeException requestFailure) {
                    request[0].complete(fallback);
                }
            });
        } catch (RuntimeException postFailure) {
            request[0].complete(fallback);
            return;
        }
        if (!posted) {
            request[0].complete(fallback);
        }
    }

    // ==================== Result builders ====================

    private static String buildLocationResult(
            int requestId, Location location, String type, boolean includeAltitude) {
        try {
            double lat = location.getLatitude();
            double lng = location.getLongitude();

            if ("gcj02".equals(type)) {
                double[] gcj = CoordinateConverter.wgs84ToGcj02(lat, lng);
                lat = gcj[0];
                lng = gcj[1];
            }

            JSONObject result = new JSONObject();
            result.put("latitude", lat);
            result.put("longitude", lng);
            result.put("speed", location.hasSpeed() ? location.getSpeed() : 0);
            result.put("accuracy", location.hasAccuracy() ? location.getAccuracy() : 0);

            if (includeAltitude) {
                result.put("altitude", location.hasAltitude() ? location.getAltitude() : 0);
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    result.put("verticalAccuracy",
                            location.hasVerticalAccuracy() ? location.getVerticalAccuracyMeters() : 0);
                } else {
                    result.put("verticalAccuracy", 0);
                }
                result.put("horizontalAccuracy", location.hasAccuracy() ? location.getAccuracy() : 0);
            } else {
                result.put("altitude", 0);
                result.put("verticalAccuracy", 0);
                result.put("horizontalAccuracy", location.hasAccuracy() ? location.getAccuracy() : 0);
            }

            CallbackCorrelation.stamp(result, requestId);
            return result.toString();
        } catch (JSONException e) {
            return errorJson(requestId, "getLocation", e.getMessage());
        }
    }

    private static String buildFuzzyResult(int requestId, Location location, String type) {
        try {
            double lat = location.getLatitude();
            double lng = location.getLongitude();

            // Apply fuzzing: round to ~1km precision (approx 0.01 degree)
            lat = Math.round(lat * 100.0) / 100.0;
            lng = Math.round(lng * 100.0) / 100.0;

            if ("gcj02".equals(type)) {
                double[] gcj = CoordinateConverter.wgs84ToGcj02(lat, lng);
                lat = gcj[0];
                lng = gcj[1];
            }

            JSONObject result = new JSONObject();
            result.put("latitude", lat);
            result.put("longitude", lng);
            CallbackCorrelation.stamp(result, requestId);
            return result.toString();
        } catch (JSONException e) {
            return errorJson(requestId, "getFuzzyLocation", e.getMessage());
        }
    }

    // ==================== Utility helpers ====================

    private static Location getBestLastKnown(LocationManager lm, boolean useGps, boolean useNetwork) {
        Location gpsLoc = useGps ? getLastKnownSafe(lm, LocationManager.GPS_PROVIDER) : null;
        Location netLoc = useNetwork ? getLastKnownSafe(lm, LocationManager.NETWORK_PROVIDER) : null;

        if (gpsLoc == null) return netLoc;
        if (netLoc == null) return gpsLoc;
        return gpsLoc.getTime() >= netLoc.getTime() ? gpsLoc : netLoc;
    }

    @SuppressWarnings("MissingPermission")
    private static Location getLastKnownSafe(LocationManager lm, String provider) {
        try {
            return lm.getLastKnownLocation(provider);
        } catch (SecurityException | IllegalArgumentException e) {
            return null;
        }
    }

    private static boolean isBetter(Location newLoc, Location oldLoc) {
        if (oldLoc == null) return true;
        long timeDelta = newLoc.getTime() - oldLoc.getTime();
        if (timeDelta > 2000) return true;
        if (timeDelta < -2000) return false;
        return newLoc.hasAccuracy() && (!oldLoc.hasAccuracy() || newLoc.getAccuracy() < oldLoc.getAccuracy());
    }

    private static boolean isStale(Location location) {
        return System.currentTimeMillis() - location.getTime() > STALE_THRESHOLD_MS;
    }

    private static boolean isProviderEnabled(LocationManager lm, String provider) {
        try {
            return lm.isProviderEnabled(provider);
        } catch (Exception e) {
            return false;
        }
    }

    private static boolean hasLocationPermission(Context context) {
        return context.checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION)
                    == PackageManager.PERMISSION_GRANTED
                || context.checkSelfPermission(Manifest.permission.ACCESS_COARSE_LOCATION)
                    == PackageManager.PERMISSION_GRANTED;
    }

    private static String errorJson(int requestId, String apiName, String reason) {
        return CallbackCorrelation.failure(requestId, apiName, reason);
    }

    private static String escapeJson(String s) {
        if (s == null) return "unknown error";
        return s.replace("\\", "\\\\").replace("\"", "\\\"");
    }
}
