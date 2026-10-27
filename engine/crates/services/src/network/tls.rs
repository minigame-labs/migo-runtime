//! The certificate policy shared by HTTPS and WSS. Mode selection happens
//! before connecting; neither path retries with another trust store on failure.

use rustls::{ClientConfig, client::danger::ServerCertVerifier, pki_types::CertificateDer};
use shared::network_policy::TlsTrust;
use std::sync::{Arc, OnceLock};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Verifier = Arc<dyn ServerCertVerifier>;

static PLATFORM: parking_lot::Mutex<Option<Verifier>> = parking_lot::Mutex::new(None);
static MOZILLA: parking_lot::Mutex<Option<Verifier>> = parking_lot::Mutex::new(None);
static PROVIDER: OnceLock<Arc<rustls::crypto::CryptoProvider>> = OnceLock::new();

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    PROVIDER
        .get_or_init(|| Arc::new(rustls::crypto::ring::default_provider()))
        .clone()
}

/// Per-client TLS sessions, lazily initialized on the shared IO executor.
/// Constructing a client or fetching plain HTTP never reads a certificate store.
#[derive(Clone)]
pub(crate) struct ClientTls(Arc<ClientTlsInner>);

struct ClientTlsInner {
    mode: TlsTrust,
    http2: bool,
    extra_roots: Vec<CertificateDer<'static>>,
    pools: migo_io::pools::IoPools,
    config: tokio::sync::OnceCell<Arc<ClientConfig>>,
}

impl ClientTls {
    pub(crate) fn new(
        mode: TlsTrust,
        http2: bool,
        extra_roots: Vec<CertificateDer<'static>>,
        pools: migo_io::pools::IoPools,
    ) -> Self {
        Self(Arc::new(ClientTlsInner {
            mode,
            http2,
            extra_roots,
            pools,
            config: tokio::sync::OnceCell::new(),
        }))
    }

    pub(crate) async fn config(&self) -> Result<Arc<ClientConfig>, Error> {
        let inner = &self.0;
        inner
            .config
            .get_or_try_init(|| async {
                let owned = self.0.clone();
                let result = inner.pools.submit_async_bytes(
                    migo_io::task::PoolKind::Tls,
                    migo_io::task::PriorityClass::ForegroundAsync,
                    1024 * 1024,
                    move || client_config(owned.mode, owned.http2, &owned.extra_roots),
                )?;
                result
                    .await
                    .map_err(|_| "TLS configuration executor closed")?
                    .map_err(|_| "TLS configuration worker panicked")?
            })
            .await
            .cloned()
    }

    pub(crate) fn pools(&self) -> migo_io::pools::IoPools {
        self.0.pools.clone()
    }
}

fn make_verifier(
    mode: TlsTrust,
    extra_roots: &[CertificateDer<'static>],
) -> Result<Verifier, Error> {
    match mode {
        TlsTrust::Mozilla => {
            let mut roots = rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            for cert in extra_roots {
                roots.add(cert.clone())?;
            }
            Ok(rustls::client::WebPkiServerVerifier::builder_with_provider(
                Arc::new(roots),
                provider(),
            )
            .build()?)
        }
        TlsTrust::Platform => {
            #[cfg(target_env = "ohos")]
            {
                let _ = extra_roots;
                Err("platform certificate verification is not supported on OHOS; select tlsTrust=mozilla explicitly".into())
            }
            #[cfg(not(target_env = "ohos"))]
            {
                #[cfg(target_os = "android")]
                {
                    if !ANDROID_INITIALIZED.load(std::sync::atomic::Ordering::Acquire) {
                        return Err("Android platform TLS needs the initialized Java SDK; native C ABI hosts without a JVM must select tlsTrust=mozilla".into());
                    }
                    if !extra_roots.is_empty() {
                        return Err("Android extra roots must be configured in the host network security policy".into());
                    }
                    Ok(Arc::new(rustls_platform_verifier::Verifier::new(
                        provider(),
                    )?))
                }
                #[cfg(not(target_os = "android"))]
                {
                    Ok(Arc::new(
                        rustls_platform_verifier::Verifier::new_with_extra_roots(
                            extra_roots.iter().cloned(),
                            provider(),
                        )?,
                    ))
                }
            }
        }
    }
}

pub(crate) fn client_config(
    mode: TlsTrust,
    enable_http2: bool,
    extra_roots: &[CertificateDer<'static>],
) -> Result<Arc<ClientConfig>, Error> {
    let verifier = if extra_roots.is_empty() {
        let mut cache = match mode {
            TlsTrust::Platform => PLATFORM.lock(),
            TlsTrust::Mozilla => MOZILLA.lock(),
        };
        if let Some(verifier) = cache.as_ref() {
            verifier.clone()
        } else {
            let verifier = make_verifier(mode, extra_roots)?;
            *cache = Some(verifier.clone());
            verifier
        }
    } else {
        make_verifier(mode, extra_roots)?
    };
    let mut config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    config.alpn_protocols = if enable_http2 {
        vec![b"h2".to_vec(), b"http/1.1".to_vec()]
    } else {
        vec![b"http/1.1".to_vec()]
    };
    Ok(Arc::new(config))
}

#[cfg(target_os = "android")]
static ANDROID_INITIALIZED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Initialize the upstream verifier from the Java SDK's application context.
/// No JNI wrapper type crosses between the SDK's JNI version and this one.
///
/// # Safety
/// `raw_env` is the current thread's live JNI environment and `raw_context`
/// is a non-null local/global Context reference valid throughout this call.
#[cfg(target_os = "android")]
pub unsafe fn init_android(
    raw_env: *mut std::ffi::c_void,
    raw_context: *mut std::ffi::c_void,
) -> Result<(), String> {
    use jni_verifier::{EnvUnowned, Outcome, objects::JObject};
    if ANDROID_INITIALIZED.load(std::sync::atomic::Ordering::Acquire) {
        return Ok(());
    }
    if raw_env.is_null() || raw_context.is_null() {
        return Err(
            "Android TLS initialization requires a live JNI environment and application context"
                .into(),
        );
    }
    // Safety: caller supplies this thread's environment and the Context remains
    // valid until init_with_env has created the upstream-owned global references.
    let mut env = unsafe { EnvUnowned::from_raw(raw_env.cast()) };
    let outcome = env
        .with_env(|env| {
            let context = unsafe { JObject::from_raw(env, raw_context.cast()) };
            rustls_platform_verifier::android::init_with_env(env, context)
        })
        .into_outcome();
    match outcome {
        Outcome::Ok(()) => {
            ANDROID_INITIALIZED.store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        }
        Outcome::Err(error) => Err(format!(
            "Android platform TLS initialization failed: {error}"
        )),
        Outcome::Panic(_) => Err("Android platform TLS initialization panicked".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::{ServerName, UnixTime};

    #[test]
    #[cfg(not(any(target_os = "android", target_env = "ohos")))]
    fn both_modes_verify_hostnames_and_never_accept_an_untrusted_certificate() {
        let cert = rcgen::generate_simple_self_signed(vec!["correct.example".into()]).unwrap();
        for mode in [TlsTrust::Platform, TlsTrust::Mozilla] {
            let trusted = make_verifier(mode, &[cert.cert.der().clone()]).unwrap();
            trusted
                .verify_server_cert(
                    cert.cert.der(),
                    &[],
                    &ServerName::try_from("correct.example").unwrap(),
                    &[],
                    UnixTime::now(),
                )
                .unwrap();
            assert!(
                trusted
                    .verify_server_cert(
                        cert.cert.der(),
                        &[],
                        &ServerName::try_from("wrong.example").unwrap(),
                        &[],
                        UnixTime::now()
                    )
                    .is_err()
            );
            let untrusted = make_verifier(mode, &[]).unwrap();
            assert!(
                untrusted
                    .verify_server_cert(
                        cert.cert.der(),
                        &[],
                        &ServerName::try_from("correct.example").unwrap(),
                        &[],
                        UnixTime::now()
                    )
                    .is_err()
            );
        }
    }

    #[test]
    #[cfg(not(any(target_os = "android", target_env = "ohos")))]
    fn configs_share_a_verifier_only_within_the_same_fixed_trust_mode() {
        for mode in [TlsTrust::Platform, TlsTrust::Mozilla] {
            assert_eq!(
                client_config(mode, false, &[]).unwrap().alpn_protocols,
                [b"http/1.1"]
            );
            assert_eq!(
                client_config(mode, true, &[]).unwrap().alpn_protocols,
                [b"h2".to_vec(), b"http/1.1".to_vec()]
            );
        }
        let platform = PLATFORM.lock().as_ref().unwrap().clone();
        let mozilla = MOZILLA.lock().as_ref().unwrap().clone();
        assert!(!Arc::ptr_eq(&platform, &mozilla));
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn a_host_ca_is_honored_only_in_the_explicit_platform_profile() {
        // Environment trust-store selection is process-wide. Exercise it in a
        // fresh process instead of racing other tests' cached TLS verifier.
        let unique = format!(
            "migo-host-ca-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let directory = std::env::temp_dir().join(unique);
        std::fs::create_dir(&directory).unwrap();
        let cert = rcgen::generate_simple_self_signed(vec!["host-ca.example".into()]).unwrap();
        let pem = directory.join("root.pem");
        let der = directory.join("leaf.der");
        std::fs::write(&pem, cert.cert.pem()).unwrap();
        std::fs::write(&der, cert.cert.der()).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "network::tls::tests::host_roots_probe",
                "--nocapture",
            ])
            .env("SSL_CERT_FILE", &pem)
            .env("SSL_CERT_DIR", &directory)
            .env("MIGO_TEST_TLS_LEAF", &der)
            .output()
            .unwrap();
        std::fs::remove_dir_all(directory).unwrap();
        assert!(
            output.status.success(),
            "host CA probe failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    #[test]
    #[ignore = "fresh-process fixture invoked by a_host_ca_is_honored_only_in_the_explicit_platform_profile"]
    fn host_roots_probe() {
        let der = CertificateDer::from(
            std::fs::read(std::env::var_os("MIGO_TEST_TLS_LEAF").expect("parent fixture")).unwrap(),
        );
        let host = ServerName::try_from("host-ca.example").unwrap();
        make_verifier(TlsTrust::Platform, &[])
            .unwrap()
            .verify_server_cert(&der, &[], &host, &[], UnixTime::now())
            .unwrap();
        assert!(
            make_verifier(TlsTrust::Mozilla, &[])
                .unwrap()
                .verify_server_cert(&der, &[], &host, &[], UnixTime::now())
                .is_err(),
            "Mozilla mode silently adopted platform roots"
        );
    }
}
