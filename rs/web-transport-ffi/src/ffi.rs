//! Shared tokio runtime used by every UniFFI-exported async method.
//!
//! A single current-thread runtime lives on a dedicated worker thread,
//! accessible via [`RUNTIME.enter()`] (to register quinn timers from `Drop`
//! paths) and via [`spawn_abortable()`] (to drive futures returned from
//! `#[uniffi::export] async fn` methods).

use std::future::Future;
use std::sync::LazyLock;

use crate::error::WebTransportError;

pub(crate) static RUNTIME: LazyLock<tokio::runtime::Handle> = LazyLock::new(|| {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build web-transport-ffi runtime");
    let handle = runtime.handle().clone();

    std::thread::Builder::new()
        .name("web-transport-ffi".into())
        .spawn(move || {
            runtime.block_on(std::future::pending::<()>());
        })
        .expect("failed to spawn web-transport-ffi runtime thread");

    handle
});

/// Spawn `fut` on [`RUNTIME`] and return a future that aborts the spawned
/// task on drop.
///
/// When the foreign-language caller cancels the returned future (e.g.
/// `asyncio.Task.cancel()`), the spawned task is aborted instead of being
/// detached. Without this, a cancelled `read()` would silently continue
/// running on the runtime thread, consume incoming data, then discard it —
/// so a subsequent `read()` would miss the bytes that were in flight when
/// cancellation happened. (See the
/// `test_asyncio_cancel_read_then_read_again` regression in the Python
/// suite.)
pub(crate) async fn spawn_abortable<F, T>(fut: F) -> Result<T, WebTransportError>
where
    F: Future<Output = Result<T, WebTransportError>> + Send + 'static,
    T: Send + 'static,
{
    let handle = RUNTIME.spawn(fut);
    let abort = handle.abort_handle();
    // Guard runs on stack drop — including when the outer future is dropped
    // because the foreign task was cancelled.
    let _guard = AbortOnDrop(abort);
    match handle.await {
        Ok(result) => result,
        Err(e) if e.is_cancelled() => Err(WebTransportError::Cancelled),
        Err(e) => Err(WebTransportError::Io(format!("task: {e}"))),
    }
}

struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
