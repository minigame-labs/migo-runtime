package com.migo.mavenconsumer;

import android.content.Context;

import com.migo.runtime.NetworkPolicy;
import com.migo.runtime.RuntimeConfig;

/** What a host writes against the published SDK: a session's configuration. */
final class HostConfig {
    private HostConfig() {}

    static RuntimeConfig forGames(Context context) {
        NetworkPolicy network = new NetworkPolicy.Builder()
                .allow(NetworkPolicy.Channel.REQUEST, "api.example.com")
                .build();
        return new RuntimeConfig.Builder(context).setNetworkPolicy(network).build();
    }
}
