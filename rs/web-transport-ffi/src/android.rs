//! Android initialization of the platform certificate verifier.
//!
//! Android's trust store is reachable only through the JVM, so
//! rustls-platform-verifier needs the application `Context` before it can
//! verify. `JNI_OnLoad` initializes it when the JVM loads this library, and each
//! default-verification client retries until it succeeds. Until then, clients
//! trust the bundled Mozilla roots from webpki-roots.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use jni::sys::{self, jint, JNI_VERSION_1_6};
use jni::{jni_sig, jni_str, Env, JavaVM};
use rustls::client::danger::ServerCertVerifier;

static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Called by `System.loadLibrary`; JNA loads skip it, so the per-client retry
/// also needs the `JavaVM` registered here.
#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn JNI_OnLoad(vm: *mut sys::JavaVM, _reserved: *mut c_void) -> jint {
    // SAFETY: the JVM passes `JNI_OnLoad` a valid `JavaVM` pointer.
    let vm = unsafe { JavaVM::from_raw(vm) };
    init(&vm);
    JNI_VERSION_1_6
}

/// The verifier for default verification on Android.
pub(crate) fn verifier(
    provider: Arc<rustls::crypto::CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, rustls::Error> {
    let ready =
        INITIALIZED.load(Ordering::Acquire) || JavaVM::singleton().is_ok_and(|vm| init(&vm));
    if ready {
        return Ok(Arc::new(rustls_platform_verifier::Verifier::new(provider)?));
    }

    tracing::warn!("Android platform verifier is not initialized; trusting bundled webpki roots");
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let verifier =
        rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider)
            .build()
            .map_err(|e| rustls::Error::General(e.to_string()))?;
    Ok(verifier)
}

/// Initialize the platform verifier, returning whether it is ready.
///
/// A failure leaves no pending Java exception: `attach_current_thread` catches
/// and clears any thrown by the callback, so loading the library never fails.
fn init(vm: &JavaVM) -> bool {
    if INITIALIZED.load(Ordering::Acquire) {
        return true;
    }
    match vm.attach_current_thread(init_with_application) {
        Ok(()) => {
            INITIALIZED.store(true, Ordering::Release);
            true
        }
        Err(err) => {
            tracing::warn!(%err, "failed to initialize the Android platform verifier");
            false
        }
    }
}

fn init_with_application(env: &mut Env) -> Result<(), jni::errors::Error> {
    // Native code isn't handed the Context, so use the long-stable
    // ActivityThread.currentApplication(), which is null until the Application exists.
    let app = env
        .call_static_method(
            jni_str!("android/app/ActivityThread"),
            jni_str!("currentApplication"),
            jni_sig!("()Landroid/app/Application;"),
            &[],
        )?
        .l()?;
    if app.is_null() {
        return Err(jni::errors::Error::NullPtr(
            "ActivityThread.currentApplication()",
        ));
    }
    rustls_platform_verifier::android::init_with_env(env, app)
}
