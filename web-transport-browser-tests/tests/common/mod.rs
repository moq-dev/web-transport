#![allow(dead_code)]

use std::time::Duration;

/// Maximum time a JS test snippet may run in Chrome before the harness aborts it.
/// Suitable for most tests (simple echo, error handling, connection lifecycle).
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Extended timeout for JS test snippets that transfer large payloads, open many
/// streams, or establish multiple sequential sessions.
pub const LONG_TIMEOUT: Duration = Duration::from_secs(30);

/// Enable `tracing` output filtered by `RUST_LOG`. Safe to call from every test —
/// `try_init` silently no-ops after the first successful initialization.
pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
}
