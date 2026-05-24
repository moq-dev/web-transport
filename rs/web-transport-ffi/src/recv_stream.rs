//! Incoming WebTransport stream wrapped for UniFFI.
//!
//! Mirrors `rs/web-transport-python/src/recv_stream.rs` — `stop()` interrupts
//! an in-flight read and sends STOP_SENDING.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::error::{
    map_read_error, map_read_exact_error, map_read_to_end_error, map_session_error,
    WebTransportError,
};
use crate::ffi::{spawn_abortable, RUNTIME};

#[derive(uniffi::Object)]
pub struct RecvStream {
    inner: Arc<Mutex<web_transport_quinn::RecvStream>>,
    cancel: CancellationToken,
    stop_code: Arc<AtomicU32>,
    eof: Arc<AtomicBool>,
}

impl RecvStream {
    pub fn new(stream: web_transport_quinn::RecvStream) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(Mutex::new(stream)),
            cancel: CancellationToken::new(),
            stop_code: Arc::new(AtomicU32::new(0)),
            eof: Arc::new(AtomicBool::new(false)),
        })
    }

    async fn cancellable<R, F, Fut>(&self, op: F) -> Result<R, WebTransportError>
    where
        R: Send + 'static,
        F: FnOnce(tokio::sync::OwnedMutexGuard<web_transport_quinn::RecvStream>) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = Result<R, WebTransportError>> + Send + 'static,
    {
        let inner = self.inner.clone();
        let cancel = self.cancel.clone();
        let stop_code = self.stop_code.clone();
        let stream = inner.clone();

        spawn_abortable(async move {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let mut g = inner.lock().await;
                    let code = stop_code.load(Ordering::Acquire);
                    let _ = g.stop(code);
                    Err(WebTransportError::StreamClosedLocally)
                }
                result = async {
                    let guard = stream.lock_owned().await;
                    op(guard).await
                } => result,
            }
        })
        .await
    }
}

#[uniffi::export]
impl RecvStream {
    /// Read up to `n` bytes from the stream.
    ///
    /// Returns an empty vector on EOF.
    pub async fn read(&self, n: u64) -> Result<Vec<u8>, WebTransportError> {
        let eof = self.eof.clone();
        self.cancellable(move |mut guard| async move {
            if n == 0 || eof.load(Ordering::Acquire) {
                return Ok(Vec::new());
            }
            let chunk = guard
                .read_chunk(n as usize, true)
                .await
                .map_err(map_read_error)?;
            match chunk {
                Some(chunk) => Ok(chunk.bytes.to_vec()),
                None => {
                    eof.store(true, Ordering::Release);
                    Ok(Vec::new())
                }
            }
        })
        .await
    }

    /// Read until EOF, capping the buffered size at `limit` bytes.
    pub async fn read_to_end(&self, limit: u64) -> Result<Vec<u8>, WebTransportError> {
        let eof = self.eof.clone();
        let limit = limit as usize;
        self.cancellable(move |mut guard| async move {
            if eof.load(Ordering::Acquire) {
                return Ok(Vec::new());
            }
            let data = guard
                .read_to_end(limit)
                .await
                .map_err(|e| map_read_to_end_error(e, limit))?;
            eof.store(true, Ordering::Release);
            Ok(data)
        })
        .await
    }

    /// Read exactly `n` bytes. Returns [`WebTransportError::StreamIncompleteRead`]
    /// on early EOF.
    pub async fn read_exact(&self, n: u64) -> Result<Vec<u8>, WebTransportError> {
        let eof = self.eof.clone();
        let n_usize = n as usize;
        self.cancellable(move |mut guard| async move {
            if n_usize > 0 && eof.load(Ordering::Acquire) {
                return Err(WebTransportError::StreamIncompleteRead {
                    expected: n,
                    got: 0,
                    partial: Vec::new(),
                });
            }
            let mut buf = vec![0u8; n_usize];
            match guard.read_exact(&mut buf).await {
                Ok(()) => Ok(buf),
                Err(e) => {
                    if matches!(e, web_transport_quinn::ReadExactError::FinishedEarly(_)) {
                        eof.store(true, Ordering::Release);
                    }
                    Err(map_read_exact_error(e, n_usize, &buf))
                }
            }
        })
        .await
    }

    /// Tell the peer to stop sending on this stream.
    #[uniffi::method(default(error_code = 0))]
    pub fn stop(&self, error_code: u32) -> Result<(), WebTransportError> {
        if self.cancel.is_cancelled() {
            return Err(WebTransportError::StreamClosedLocally);
        }
        self.stop_code.store(error_code, Ordering::Release);
        self.cancel.cancel();
        let _guard = RUNTIME.enter();
        if let Ok(mut g) = self.inner.try_lock() {
            let _ = g.stop(error_code);
        }
        Ok(())
    }

    /// Wait until the peer resets the stream or sends FIN.
    ///
    /// Returns the peer's RESET_STREAM error code, or `None` if the stream
    /// ended cleanly.
    pub async fn wait_closed(&self) -> Result<Option<u32>, WebTransportError> {
        self.cancellable(|mut guard| async move {
            guard.received_reset().await.map_err(map_session_error)
        })
        .await
    }
}
