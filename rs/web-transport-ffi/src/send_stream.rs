//! Outgoing WebTransport stream wrapped for UniFFI.
//!
//! Mirrors `rs/web-transport-python/src/send_stream.rs` — the cancellable
//! write pattern is preserved so `reset()` can interrupt an in-flight
//! `write()`/`finish()` and send RESET_STREAM.

use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::error::{map_session_error, map_write_error, WebTransportError};
use crate::ffi::{spawn_abortable, RUNTIME};

#[derive(uniffi::Object)]
pub struct SendStream {
    inner: Arc<Mutex<web_transport_quinn::SendStream>>,
    cancel: CancellationToken,
    reset_code: Arc<AtomicU32>,
    priority: Arc<AtomicI32>,
    synced_priority: Arc<AtomicI32>,
}

impl SendStream {
    pub fn new(stream: web_transport_quinn::SendStream) -> Arc<Self> {
        let priority = stream.priority().unwrap_or(0);
        Arc::new(Self {
            inner: Arc::new(Mutex::new(stream)),
            cancel: CancellationToken::new(),
            reset_code: Arc::new(AtomicU32::new(0)),
            priority: Arc::new(AtomicI32::new(priority)),
            synced_priority: Arc::new(AtomicI32::new(priority)),
        })
    }

    async fn cancellable<R, F, Fut>(&self, op: F) -> Result<R, WebTransportError>
    where
        R: Send + 'static,
        F: FnOnce(tokio::sync::OwnedMutexGuard<web_transport_quinn::SendStream>) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = Result<R, WebTransportError>> + Send + 'static,
    {
        let inner = self.inner.clone();
        let cancel = self.cancel.clone();
        let reset_code = self.reset_code.clone();
        let priority = self.priority.clone();
        let synced_priority = self.synced_priority.clone();
        let stream = inner.clone();

        spawn_abortable(async move {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let mut g = inner.lock().await;
                    let code = reset_code.load(Ordering::Acquire);
                    let _ = g.reset(code);
                    Err(WebTransportError::StreamClosedLocally)
                }
                result = async {
                    let guard = stream.lock_owned().await;
                    let desired = priority.load(Ordering::Relaxed);
                    if desired != synced_priority.load(Ordering::Relaxed) {
                        let _ = guard.set_priority(desired);
                        synced_priority.store(desired, Ordering::Relaxed);
                    }
                    op(guard).await
                } => result,
            }
        })
        .await
    }
}

#[uniffi::export]
impl SendStream {
    /// Write all of `data` to the stream.
    pub async fn write(&self, data: Vec<u8>) -> Result<(), WebTransportError> {
        self.cancellable(|mut guard| async move {
            guard.write_all(&data).await.map_err(map_write_error)
        })
        .await
    }

    /// Write some of `data`, returning the number of bytes written.
    pub async fn write_some(&self, data: Vec<u8>) -> Result<u64, WebTransportError> {
        self.cancellable(|mut guard| async move {
            guard
                .write(&data)
                .await
                .map(|n| n as u64)
                .map_err(map_write_error)
        })
        .await
    }

    /// Gracefully close the stream (sends FIN).
    pub async fn finish(&self) -> Result<(), WebTransportError> {
        self.cancellable(|mut guard| async move {
            guard
                .finish()
                .map_err(|_| WebTransportError::StreamClosedLocally)
        })
        .await
    }

    /// Abruptly reset the stream with the given application error code.
    #[uniffi::method(default(error_code = 0))]
    pub fn reset(&self, error_code: u32) -> Result<(), WebTransportError> {
        if self.cancel.is_cancelled() {
            return Err(WebTransportError::StreamClosedLocally);
        }
        self.reset_code.store(error_code, Ordering::Release);
        self.cancel.cancel();
        let _guard = RUNTIME.enter();
        if let Ok(mut g) = self.inner.try_lock() {
            let _ = g.reset(error_code);
        }
        Ok(())
    }

    /// Wait until the peer stops the stream or reads it to completion.
    ///
    /// Returns the peer's STOP_SENDING error code, or `None` if the stream
    /// was finished cleanly.
    pub async fn wait_closed(&self) -> Result<Option<u32>, WebTransportError> {
        self.cancellable(|guard| async move { guard.stopped().await.map_err(map_session_error) })
            .await
    }

    /// Stream scheduling priority (higher = sent first).
    pub fn priority(&self) -> i32 {
        self.priority.load(Ordering::Relaxed)
    }

    pub fn set_priority(&self, value: i32) {
        self.priority.store(value, Ordering::Relaxed);
        if let Ok(g) = self.inner.try_lock() {
            let _ = g.set_priority(value);
            self.synced_priority.store(value, Ordering::Relaxed);
        }
    }
}
