package com.migo.runtime;

import java.util.ArrayList;
import java.util.EnumMap;
import java.util.EnumSet;
import java.util.List;
import java.util.Locale;
import java.util.Map;

/**
 * What network a game may reach: one allowlist per channel, and nothing that is
 * not granted.
 * <p>
 * A {@link RuntimeConfig} without a policy grants nothing at all -- no request,
 * no download, no socket. Grant each channel the domains the game needs:
 *
 * <pre>
 * NetworkPolicy policy = new NetworkPolicy.Builder()
 *         .allow(NetworkPolicy.Channel.REQUEST, "api.example.com")
 *         .allow(NetworkPolicy.Channel.DOWNLOAD, "*.cdn.example.com")
 *         .build();
 * </pre>
 *
 * An entry is a domain ({@code api.example.com}, that host only) or a wildcard
 * ({@code *.example.com}, its subdomains and not {@code example.com} itself).
 * IP addresses cannot be listed: content reaches servers by name. Plain
 * {@code http://} and {@code ws://}, and other devices on the local network,
 * are refused unless switched on.
 */
public final class NetworkPolicy {

    /** Fixed certificate trust for both HTTPS and WSS. Verification never falls back. */
    public enum TlsTrust {
        /** Android trust manager, including the host's network security configuration. */
        PLATFORM("platform"),
        /** The engine's pinned Mozilla root set. */
        MOZILLA("mozilla");

        final String key;
        TlsTrust(String key) { this.key = key; }
    }

    /** The kinds of traffic a policy grants separately. */
    public enum Channel {
        /** {@code request} / {@code fetch}. */
        REQUEST("request"),
        /**
         * {@code downloadFile}, remote images, streamed audio, asset prefetch, and a URL
         * given to {@code previewImage}, {@code previewMedia}, a share, a video or a video decoder.
         */
        DOWNLOAD("download"),
        /** {@code uploadFile}. */
        UPLOAD("upload"),
        /** WebSocket. */
        SOCKET("socket"),
        /** Raw TCP. */
        TCP("tcp"),
        /** Raw UDP. */
        UDP("udp");

        final String key;

        Channel(String key) {
            this.key = key;
        }
    }

    private final String json;

    private NetworkPolicy(String json) {
        this.json = json;
    }

    /** Every channel refused: what a {@link RuntimeConfig} has until it is given a policy. */
    public static NetworkPolicy denyAll() {
        return new Builder().build();
    }

    /** The form the engine parses. */
    String toJson() {
        return json;
    }

    @Override
    public String toString() {
        return "NetworkPolicy" + json;
    }

    /** Builds a {@link NetworkPolicy}; each entry is checked when it is added. */
    public static final class Builder {
        private final Map<Channel, List<String>> allowlists = new EnumMap<>(Channel.class);
        private final EnumSet<Channel> unrestricted = EnumSet.noneOf(Channel.class);
        private boolean allowPlainHttp = false;
        private boolean localNetwork = false;
        private TlsTrust tlsTrust = TlsTrust.PLATFORM;

        /**
         * Grant {@code channel} these domains, in addition to any granted before. Every
         * entry is checked before any is granted, so a refused call grants nothing.
         *
         * @throws IllegalArgumentException if an entry is not a domain or a {@code *.} wildcard
         * @throws IllegalStateException if the channel is already unrestricted
         */
        public Builder allow(Channel channel, String... domains) {
            requireChannel(channel);
            if (unrestricted.contains(channel)) {
                throw new IllegalStateException(channel + " is already unrestricted");
            }
            if (domains == null || domains.length == 0) {
                throw new IllegalArgumentException("allow(" + channel + ") needs at least one domain");
            }
            List<String> granted = new ArrayList<>(domains.length);
            for (String domain : domains) {
                granted.add(normalize(domain));
            }
            List<String> list = allowlists.get(channel);
            if (list == null) {
                allowlists.put(channel, granted);
            } else {
                list.addAll(granted);
            }
            return this;
        }

        /**
         * Grant {@code channel} every domain. For developer tools, or a host that has
         * already vetted the content it runs. An IP address is still refused, but for
         * another device on the local network when {@link #setAllowLocalNetwork} is on.
         *
         * @throws IllegalStateException if domains were already granted to the channel
         */
        public Builder allowUnrestricted(Channel channel) {
            requireChannel(channel);
            if (allowlists.containsKey(channel)) {
                throw new IllegalStateException(channel + " already has an allowlist");
            }
            unrestricted.add(channel);
            return this;
        }

        /** Allow {@code http://} and {@code ws://} to the granted domains. Default: off. */
        public Builder setAllowPlainHttp(boolean allow) {
            this.allowPlainHttp = allow;
            return this;
        }

        /**
         * Let the socket, TCP and UDP channels reach other devices on the device's
         * own local network -- for LAN play. A request, download or upload never
         * reaches one. Default: off.
         */
        public Builder setAllowLocalNetwork(boolean allow) {
            this.localNetwork = allow;
            return this;
        }

        /** Choose the trust store before connecting. Default: Android platform trust. */
        public Builder setTlsTrust(TlsTrust trust) {
            if (trust == null) throw new IllegalArgumentException("TLS trust must not be null");
            this.tlsTrust = trust;
            return this;
        }

        public NetworkPolicy build() {
            StringBuilder out = new StringBuilder("{");
            for (Channel channel : Channel.values()) {
                String value;
                if (unrestricted.contains(channel)) {
                    value = "\"unrestricted\"";
                } else if (allowlists.containsKey(channel)) {
                    StringBuilder list = new StringBuilder("[");
                    for (String domain : allowlists.get(channel)) {
                        if (list.length() > 1) list.append(',');
                        // An entry is letters, digits, '-', '.' and a leading '*': nothing to escape.
                        list.append('"').append(domain).append('"');
                    }
                    value = list.append(']').toString();
                } else {
                    continue;
                }
                if (out.length() > 1) out.append(',');
                out.append('"').append(channel.key).append("\":").append(value);
            }
            if (allowPlainHttp) {
                if (out.length() > 1) out.append(',');
                out.append("\"allowPlainHttp\":true");
            }
            if (localNetwork) {
                if (out.length() > 1) out.append(',');
                out.append("\"lan\":true");
            }
            if (tlsTrust != TlsTrust.PLATFORM) {
                if (out.length() > 1) out.append(',');
                out.append("\"tlsTrust\":\"").append(tlsTrust.key).append('"');
            }
            return new NetworkPolicy(out.append('}').toString());
        }

        private static void requireChannel(Channel channel) {
            if (channel == null) {
                throw new IllegalArgumentException("channel must not be null");
            }
        }
    }

    /**
     * An entry in the form it is stored in, or a refusal. The engine parses the same
     * grammar; both are held to {@code contracts/runtime/domain-patterns.json}.
     */
    static String normalize(String entry) {
        if (entry == null) {
            throw new IllegalArgumentException("a domain must not be null");
        }
        boolean wildcard = entry.startsWith("*.");
        String domain = wildcard ? entry.substring(2) : entry;
        if (domain.isEmpty()) {
            throw refused(entry, "is empty");
        }
        for (int i = 0; i < domain.length(); i++) {
            if (domain.charAt(i) > 0x7f) {
                throw refused(entry, "must be ASCII; write an internationalised name in its xn-- form");
            }
        }
        domain = domain.toLowerCase(Locale.ROOT);
        String[] labels = domain.split("\\.", -1);
        if (labels.length < 2) {
            throw refused(entry, "needs at least two labels, e.g. example.com");
        }
        for (String label : labels) {
            if (label.isEmpty() || label.length() > 63) {
                throw refused(entry, "has an empty or over-long label");
            }
            for (int i = 0; i < label.length(); i++) {
                char c = label.charAt(i);
                boolean ok = (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '-';
                if (!ok) {
                    throw refused(entry,
                            "may contain only letters, digits, hyphens and dots, with '*.' only at the start");
                }
            }
            if (label.startsWith("-") || label.endsWith("-")) {
                throw refused(entry, "has a label that starts or ends with a hyphen");
            }
        }
        // The URL standard reads a host whose last label is a number as an IPv4 address.
        String last = labels[labels.length - 1];
        if (last.matches("[0-9]+") || last.matches("0x[0-9a-f]*")) {
            throw refused(entry, "is an IP address; allowlists name domains");
        }
        return wildcard ? "*." + domain : domain;
    }

    private static IllegalArgumentException refused(String entry, String why) {
        return new IllegalArgumentException("domain pattern '" + entry + "': " + why);
    }
}
