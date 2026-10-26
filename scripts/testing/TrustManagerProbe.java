// Local selector probe, not a certificate validator or Android device test.
// All providers/principals are dummy, and the harness runs without a network.
import java.io.InputStream;
import java.io.OutputStream;
import java.lang.reflect.Field;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.security.Key;
import java.security.KeyStore;
import java.security.KeyStoreSpi;
import java.security.Provider;
import java.security.Security;
import java.security.cert.Certificate;
import java.security.cert.X509Certificate;
import java.util.Collections;
import java.util.Date;
import java.util.Enumeration;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.atomic.AtomicReference;
import javax.net.ssl.ManagerFactoryParameters;
import javax.net.ssl.TrustManager;
import javax.net.ssl.TrustManagerFactorySpi;
import javax.net.ssl.X509TrustManager;

public final class TrustManagerProbe {
    public static final class DummyStore extends KeyStoreSpi {
        public Key engineGetKey(String alias, char[] password) { return null; }
        public Certificate[] engineGetCertificateChain(String alias) { return null; }
        public Certificate engineGetCertificate(String alias) { return null; }
        public Date engineGetCreationDate(String alias) { return null; }
        public void engineSetKeyEntry(String alias, Key key, char[] password, Certificate[] chain) {}
        public void engineSetKeyEntry(String alias, byte[] key, Certificate[] chain) {}
        public void engineSetCertificateEntry(String alias, Certificate certificate) {}
        public void engineDeleteEntry(String alias) {}
        public Enumeration<String> engineAliases() { return Collections.emptyEnumeration(); }
        public boolean engineContainsAlias(String alias) { return false; }
        public int engineSize() { return 0; }
        public boolean engineIsKeyEntry(String alias) { return false; }
        public boolean engineIsCertificateEntry(String alias) { return false; }
        public String engineGetCertificateAlias(Certificate certificate) { return null; }
        public void engineStore(OutputStream output, char[] password) {}
        public void engineLoad(InputStream input, char[] password) {}
    }

    public static final class SpyFactory extends TrustManagerFactorySpi {
        static KeyStore received;
        static int calls;
        protected void engineInit(KeyStore store) { received = store; calls++; }
        protected void engineInit(ManagerFactoryParameters parameters) { throw new AssertionError(); }
        protected TrustManager[] engineGetTrustManagers() {
            return new TrustManager[] { new X509TrustManager() {
                public void checkClientTrusted(X509Certificate[] chain, String type) { throw new AssertionError("no certificate inputs in this probe"); }
                public void checkServerTrusted(X509Certificate[] chain, String type) { throw new AssertionError("no certificate inputs in this probe"); }
                public X509Certificate[] getAcceptedIssuers() { return new X509Certificate[0]; }
            } };
        }
    }

    private static void checkFactoryOwnership(Class<?> verifier) throws Exception {
        Field field = verifier.getDeclaredField("certFactory");
        field.setAccessible(true);
        Object owner = field.get(null);
        if (!(owner instanceof ThreadLocal)) throw new AssertionError("CertificateFactory must be owned per worker");
        ThreadLocal<?> local = (ThreadLocal<?>) owner;
        Object[] factories = new Object[2];
        CountDownLatch ready = new CountDownLatch(2);
        AtomicReference<Throwable> failed = new AtomicReference<>();
        Thread[] threads = new Thread[2];
        for (int i = 0; i < threads.length; i++) {
            final int slot = i;
            threads[i] = new Thread(() -> {
                try {
                    factories[slot] = local.get();
                    ready.countDown();
                    ready.await();
                    if (factories[slot] != local.get()) throw new AssertionError("worker did not reuse its factory");
                } catch (Throwable error) {
                    failed.compareAndSet(null, error);
                } finally {
                    ready.countDown();
                }
            });
            threads[i].start();
        }
        for (Thread thread : threads) thread.join(5000);
        for (Thread thread : threads) if (thread.isAlive()) throw new AssertionError("factory worker did not finish");
        if (failed.get() != null) throw new AssertionError(failed.get());
        if (factories[0] == factories[1]) throw new AssertionError("workers shared a factory");
        System.out.println("factory_ownership=per_thread");
    }

    @SuppressWarnings("deprecation")
    public static void main(String[] args) throws Exception {
        Provider provider = new Provider("MigoSelectorProbe", 1.0, "dummy, no actual trust") {};
        provider.put("KeyStore.AndroidCAStore", DummyStore.class.getName());
        provider.put("TrustManagerFactory.MigoSelectorProbe", SpyFactory.class.getName());
        Security.addProvider(provider);
        Security.setProperty("ssl.TrustManagerFactory.algorithm", "MigoSelectorProbe");
        Class<?> verifier = Class.forName("org.rustls.platformverifier.CertificateVerifier");
        Field field = verifier.getDeclaredField("systemKeystore");
        field.setAccessible(true);
        KeyStore systemStore = (KeyStore) field.get(null);
        if (systemStore == null || !systemStore.getType().equals("AndroidCAStore")) {
            throw new AssertionError("dummy AndroidCAStore must be selected; otherwise this is a vacuous probe");
        }
        Method method = verifier.getDeclaredMethod("makeLazyTrustManager", KeyStore.class);
        method.setAccessible(true);
        try {
            // Original release bytecode has a static method; source builds may
            // retain the Kotlin object receiver. Neither path validates a cert.
            Object receiver = java.lang.reflect.Modifier.isStatic(method.getModifiers()) ? null
                    : verifier.getField("INSTANCE").get(null);
            Object lazy = method.invoke(receiver, systemStore);
            Class.forName("kotlin.Lazy").getMethod("getValue").invoke(lazy);
        } catch (InvocationTargetException expectedStub) {
            // Android's X509TrustManagerExtensions constructor is a stub here.
            // The selector already called the real JCA SPI by this point.
        }
        if (SpyFactory.calls != 1) throw new AssertionError("selector was not exercised exactly once");
        String observed = SpyFactory.received == null ? "default_host_config" : "explicit_keystore";
        System.out.println("factory_selection=" + observed);
        if ((args.length != 1 && args.length != 2) || !args[0].equals(observed)) throw new AssertionError("unexpected selector");
        if (args.length == 2 && args[1].equals("per_thread_factory")) checkFactoryOwnership(verifier);
    }
}
