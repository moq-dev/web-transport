//! Android initialization of the platform certificate verifier.
//!
//! rustls-platform-verifier reaches Android's trust store through the JVM and
//! panics during verification if it was never given the application `Context`.
//! It does not expose whether it is initialized, so [`init_platform_verifier`]
//! initializes it idempotently and returns an error when that is impossible.

use std::sync::atomic::{AtomicBool, Ordering};

use jni::{jni_sig, jni_str, Env, JavaVM};

static READY: AtomicBool = AtomicBool::new(false);

/// Ensure the platform verifier is initialized, initializing it from the
/// application `Context` when the application has not.
pub fn init_platform_verifier() -> Result<(), rustls::Error> {
    if READY.load(Ordering::Acquire) {
        return Ok(());
    }

    // Every rustls-platform-verifier initializer takes a value from this `jni`
    // version, and creating any of them registers the VM singleton. Without a
    // singleton the verifier cannot have been initialized.
    let vm = JavaVM::singleton().map_err(uninitialized)?;
    vm.attach_current_thread_for_scope(init_with_application)
        .map_err(uninitialized)?;

    READY.store(true, Ordering::Release);
    Ok(())
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

    // A no-op when the application already initialized the verifier.
    rustls_platform_verifier::android::init_with_env(env, app)
}

fn uninitialized(err: jni::errors::Error) -> rustls::Error {
    rustls::Error::General(format!(
        "the Android platform verifier is not initialized and could not be \
         initialized from the application Context ({err}); see \
         https://github.com/rustls/rustls-platform-verifier#android"
    ))
}
