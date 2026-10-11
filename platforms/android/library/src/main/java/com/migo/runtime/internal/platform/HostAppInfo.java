package com.migo.runtime.internal.platform;

import android.content.Context;
import android.content.res.Configuration;
import android.os.LocaleList;

import com.migo.runtime.RuntimeConfig;

import org.json.JSONException;
import org.json.JSONObject;

import java.util.Locale;

/**
 * What the app says about itself and the content it runs: the report the
 * engine composes {@code getAppBaseInfo}, {@code getAccountInfoSync} and
 * {@code getDeviceBenchmarkInfo} from, beside what it knows itself (the API
 * level it implements, its own version, the content's id).
 * <p>
 * The app's id, language and text scale are the app's own, read from its
 * resources' configuration -- the locale its UI is shown in, which may be one
 * the app chose rather than the system's first. The content's build and
 * version, and any assessment of the device, are what the app configured;
 * left unconfigured they are a release with no version on a device of unknown
 * performance, never a guess.
 *
 * @hide
 */
public final class HostAppInfo {

    private HostAppInfo() {}

    /**
     * The report, as JSON.
     *
     * @param context the session's context, or null when it has none
     * @param config  the session's configuration
     * @param debug   whether the debugging aids are on
     */
    public static String toJson(Context context, RuntimeConfig config, boolean debug) {
        JSONObject json = new JSONObject();
        try {
            if (context != null) {
                json.put("hostAppId", context.getPackageName());
                Configuration configuration = context.getResources().getConfiguration();
                LocaleList locales = configuration.getLocales();
                if (!locales.isEmpty()) {
                    json.put("language", locales.get(0).toLanguageTag());
                }
                if (configuration.fontScale > 0f) {
                    // The float's shortest decimal, so 1.15 reaches content as 1.15.
                    json.put("fontScale", Double.parseDouble(Float.toString(configuration.fontScale)));
                }
            }
            json.put("debug", debug);
            json.put("benchmarkLevel", config.getBenchmarkLevel());
            json.put("modelLevel", config.getModelLevel());
            json.put("contentEnvironment",
                    config.getContentEnvironment().name().toLowerCase(Locale.ROOT));
            String version = config.getContentVersion();
            if (version != null) {
                json.put("contentVersion", version);
            }
        } catch (JSONException e) {
            // Only a non-finite number throws, and none is put above.
            throw new IllegalStateException(e);
        }
        return json.toString();
    }
}
