package com.migo.runtime;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.assertThrows;

import android.content.Context;
import java.lang.reflect.Method;
import java.lang.reflect.Modifier;
import java.net.URL;
import java.net.URLClassLoader;
import org.junit.Test;

/** Source-derived JNI descriptors/runtime closure; trust-manager behavior needs a device. */
public final class PlatformTlsBytecodeTest {
    @Test
    public void upstream_jni_descriptors_and_kotlin_runtime_resolve() throws Exception {
        ClassLoader loader = getClass().getClassLoader();
        Class<?> verifier = Class.forName("org.rustls.platformverifier.CertificateVerifier", false, loader);
        Class<?> result = Class.forName("org.rustls.platformverifier.VerificationResult", false, loader);
        Class<?> status = Class.forName("org.rustls.platformverifier.StatusCode", false, loader);
        Method method = verifier.getDeclaredMethod("verifyCertificateChain", Context.class,
                String.class, String.class, String[].class, byte[].class, long.class, byte[][].class);
        assertTrue(Modifier.isStatic(method.getModifiers()));
        assertEquals(result, method.getReturnType());
        assertEquals(int.class, result.getDeclaredField("code").getType());
        assertEquals(String.class, result.getDeclaredField("message").getType());
        assertTrue(status.isEnum());
        // Resolving the actual descriptors catches an omitted/incompatible Kotlin
        // runtime that an Android Java-only compilation does not otherwise see.
        verifier.getDeclaredFields();
        status.getDeclaredMethods();
        result.getDeclaredConstructors();
        Class.forName("kotlin.Lazy", false, loader);
        Class.forName("kotlin.enums.EnumEntries", false, loader);
    }

    @Test
    public void omitting_the_runtime_really_breaks_the_upstream_bytecode() throws Exception {
        Class<?> verifier = Class.forName("org.rustls.platformverifier.CertificateVerifier", false,
                getClass().getClassLoader());
        URL jar = verifier.getProtectionDomain().getCodeSource().getLocation();
        try (URLClassLoader isolated = new URLClassLoader(new URL[] { jar }, null)) {
            Class<?> missingRuntime = Class.forName(verifier.getName(), false, isolated);
            NoClassDefFoundError error = assertThrows(NoClassDefFoundError.class,
                    () -> missingRuntime.getDeclaredFields());
            assertTrue(error.getMessage(), error.getMessage().contains("kotlin"));
        }
    }
}
