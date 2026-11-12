//! Android initialization of the platform certificate verifier.
//!
//! Android's trust store is reachable only through the JVM, so
//! rustls-platform-verifier needs the application `Context` before it can
//! verify. `JNI_OnLoad` initializes it when the JVM loads this library, and each
//! default-verification client retries until it succeeds. Until then, clients
//! trust the bundled Mozilla roots from webpki-roots.

use std::ffi::c_void;
use std::sync::Arc;

use jni::sys::{self, jint, JNI_VERSION_1_6};
use jni::JavaVM;
use rustls::client::danger::ServerCertVerifier;
use web_transport_quinn::android::init_platform_verifier;

/// Called by `System.loadLibrary`; JNA loads skip it, so the per-client retry
/// also needs the `JavaVM` singleton registered here.
#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn JNI_OnLoad(vm: *mut sys::JavaVM, _reserved: *mut c_void) -> jint {
    // SAFETY: the JVM passes `JNI_OnLoad` a valid `JavaVM` pointer.
    unsafe { JavaVM::from_raw(vm) };
    // A failure leaves no pending Java exception, so loading the library never fails.
    if let Err(err) = init_platform_verifier() {
        tracing::warn!(%err, "failed to initialize the Android platform verifier");
    }
    JNI_VERSION_1_6
}

/// The verifier for default verification on Android.
pub(crate) fn verifier(
    provider: Arc<rustls::crypto::CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, rustls::Error> {
    match init_platform_verifier() {
        Ok(()) => return Ok(Arc::new(rustls_platform_verifier::Verifier::new(provider)?)),
        Err(err) => tracing::warn!(%err, "trusting bundled webpki roots"),
    }

    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let verifier =
        rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider)
            .build()
            .map_err(|e| rustls::Error::General(e.to_string()))?;
    Ok(verifier)
}
