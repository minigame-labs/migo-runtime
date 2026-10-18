package com.migo.runtime;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.fail;

import org.json.JSONArray;
import org.json.JSONObject;
import org.junit.Test;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;

/**
 * The builder refuses at the call site exactly what the engine would refuse, and writes
 * what the engine reads: both are held to {@code contracts/runtime/domain-patterns.json}.
 */
public final class NetworkPolicyTest {

    private static final String CORPUS = "contracts/runtime/domain-patterns.json";

    @Test
    public void the_builder_holds_to_the_shared_grammar() throws Exception {
        JSONObject corpus = corpus();
        JSONArray accepted = corpus.getJSONArray("accepted");
        for (int i = 0; i < accepted.length(); i++) {
            JSONArray pair = accepted.getJSONArray(i);
            assertEquals(pair.getString(0), pair.getString(1), NetworkPolicy.normalize(pair.getString(0)));
        }
        JSONArray refused = corpus.getJSONArray("refused");
        for (int i = 0; i < refused.length(); i++) {
            String entry = refused.getString(i);
            try {
                NetworkPolicy.normalize(entry);
                fail(entry + " was accepted");
            } catch (IllegalArgumentException expected) {
                assertTrue(expected.getMessage(), expected.getMessage().contains("'" + entry + "'"));
            }
        }
    }

    @Test
    public void the_builder_writes_what_the_engine_reads() throws Exception {
        NetworkPolicy policy = new NetworkPolicy.Builder()
                .allow(NetworkPolicy.Channel.REQUEST, "API.example.com")
                .allow(NetworkPolicy.Channel.DOWNLOAD, "*.cdn.example.com")
                .allowUnrestricted(NetworkPolicy.Channel.SOCKET)
                .setAllowPlainHttp(true)
                .setAllowLocalNetwork(true)
                .build();
        assertEquals(corpus().getString("builderSample"), policy.toJson());
    }

    @Test
    public void nothing_is_granted_unless_it_is_set() {
        assertEquals("{}", NetworkPolicy.denyAll().toJson());
        assertEquals("{}", new NetworkPolicy.Builder().build().toJson());
    }

    @Test
    public void a_channel_is_either_listed_or_unrestricted_never_both() {
        assertThrows(IllegalStateException.class, () -> new NetworkPolicy.Builder()
                .allow(NetworkPolicy.Channel.TCP, "game.example.com")
                .allowUnrestricted(NetworkPolicy.Channel.TCP));
        assertThrows(IllegalStateException.class, () -> new NetworkPolicy.Builder()
                .allowUnrestricted(NetworkPolicy.Channel.TCP)
                .allow(NetworkPolicy.Channel.TCP, "game.example.com"));
        assertThrows(IllegalArgumentException.class, () -> new NetworkPolicy.Builder()
                .allow(NetworkPolicy.Channel.TCP));
        assertThrows(IllegalArgumentException.class, () -> new NetworkPolicy.Builder()
                .allow(null, "game.example.com"));
    }

    private static JSONObject corpus() throws Exception {
        StringBuilder searched = new StringBuilder();
        Path directory = Paths.get("").toAbsolutePath();
        while (directory != null) {
            Path candidate = directory.resolve(CORPUS);
            if (Files.isReadable(candidate)) {
                return new JSONObject(new String(Files.readAllBytes(candidate), StandardCharsets.UTF_8));
            }
            searched.append("\n  ").append(candidate);
            directory = directory.getParent();
        }
        throw new AssertionError("domain-pattern corpus not found, searched:" + searched);
    }
}
