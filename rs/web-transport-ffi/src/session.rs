//! WebTransport session wrapped for UniFFI.
//!
//! Wraps `web_transport_quinn::Session` in `Option` so [`Drop`] can release
//! quinn-owned resources inside the [`RUNTIME`] context (see moq ae506266
//! for the bug this avoids).

use std::sync::Arc;

use crate::error::{map_session_error, WebTransportError};
use crate::ffi::RUNTIME;
use crate::recv_stream::RecvStream;
use crate::send_stream::SendStream;

/// One peer of a WebTransport session.
///
/// All async methods drive their futures on the FFI runtime so the foreign
/// language can poll them without setting up a tokio context of its own.
#[derive(uniffi::Object)]
pub struct Session {
    inner: tokio::sync::Mutex<Option<web_transport_quinn::Session>>,
    clone_handle: web_transport_quinn::Session,
}

impl Session {
    pub fn new(session: web_transport_quinn::Session) -> Arc<Self> {
        Arc::new(Self {
            inner: tokio::sync::Mutex::new(Some(session.clone())),
            clone_handle: session,
        })
    }

    async fn session(&self) -> Result<web_transport_quinn::Session, WebTransportError> {
        let guard = self.inner.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or(WebTransportError::SessionClosedLocally)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _guard = RUNTIME.enter();
        // Drop the held Session inside the runtime so quinn can register
        // its connection-close timer.
        if let Ok(mut g) = self.inner.try_lock() {
            g.take();
        }
    }
}

#[uniffi::export]
impl Session {
    /// Open a new bidirectional stream.
    pub async fn open_bi(&self) -> Result<BiStream, WebTransportError> {
        let session = self.session().await?;
        let handle =
            RUNTIME.spawn(async move { session.open_bi().await.map_err(map_session_error) });
        let (send, recv) = handle
            .await
            .map_err(|e| WebTransportError::Io(format!("open_bi task: {e}")))??;
        Ok(BiStream {
            send: SendStream::new(send),
            recv: RecvStream::new(recv),
        })
    }

    /// Open a new unidirectional (send-only) stream.
    pub async fn open_uni(&self) -> Result<Arc<SendStream>, WebTransportError> {
        let session = self.session().await?;
        let handle =
            RUNTIME.spawn(async move { session.open_uni().await.map_err(map_session_error) });
        let send = handle
            .await
            .map_err(|e| WebTransportError::Io(format!("open_uni task: {e}")))??;
        Ok(SendStream::new(send))
    }

    /// Accept the next bidirectional stream opened by the peer.
    pub async fn accept_bi(&self) -> Result<BiStream, WebTransportError> {
        let session = self.session().await?;
        let handle =
            RUNTIME.spawn(async move { session.accept_bi().await.map_err(map_session_error) });
        let (send, recv) = handle
            .await
            .map_err(|e| WebTransportError::Io(format!("accept_bi task: {e}")))??;
        Ok(BiStream {
            send: SendStream::new(send),
            recv: RecvStream::new(recv),
        })
    }

    /// Accept the next unidirectional stream opened by the peer.
    pub async fn accept_uni(&self) -> Result<Arc<RecvStream>, WebTransportError> {
        let session = self.session().await?;
        let handle =
            RUNTIME.spawn(async move { session.accept_uni().await.map_err(map_session_error) });
        let recv = handle
            .await
            .map_err(|e| WebTransportError::Io(format!("accept_uni task: {e}")))??;
        Ok(RecvStream::new(recv))
    }

    /// Send an unreliable datagram. Fails if the payload exceeds
    /// [`Self::max_datagram_size`].
    pub fn send_datagram(&self, data: Vec<u8>) -> Result<(), WebTransportError> {
        let _guard = RUNTIME.enter();
        self.clone_handle
            .send_datagram(bytes::Bytes::from(data))
            .map_err(map_session_error)
    }

    /// Wait for and return the next incoming datagram.
    pub async fn receive_datagram(&self) -> Result<Vec<u8>, WebTransportError> {
        let session = self.session().await?;
        let handle =
            RUNTIME.spawn(async move { session.read_datagram().await.map_err(map_session_error) });
        let data = handle
            .await
            .map_err(|e| WebTransportError::Io(format!("receive_datagram task: {e}")))??;
        Ok(data.to_vec())
    }

    /// Close the session.
    #[uniffi::method(default(code = 0, reason = ""))]
    pub fn close(&self, code: u32, reason: String) {
        let _guard = RUNTIME.enter();
        self.clone_handle.close(code, reason.as_bytes());
    }

    /// Wait until the session is closed (for any reason).
    pub async fn wait_closed(&self) {
        let session = self.clone_handle.clone();
        let handle = RUNTIME.spawn(async move {
            let _ = session.closed().await;
        });
        let _ = handle.await;
    }

    /// Whether the session has been closed.
    pub fn is_closed(&self) -> bool {
        self.clone_handle.close_reason().is_some()
    }

    /// If the session has been closed, return the close error structured as
    /// a [`WebTransportError`]. Returns `None` while the session is alive.
    pub fn close_reason(&self) -> Option<WebTransportError> {
        self.clone_handle
            .close_reason()
            .map(crate::error::map_session_error)
    }

    /// Maximum payload size accepted by [`Self::send_datagram`].
    pub fn max_datagram_size(&self) -> u64 {
        self.clone_handle.max_datagram_size() as u64
    }

    /// Remote peer address as a `(host, port)` tuple.
    pub fn remote_address(&self) -> RemoteAddress {
        let addr = self.clone_handle.remote_address();
        RemoteAddress {
            host: addr.ip().to_string(),
            port: addr.port(),
        }
    }

    /// Current estimated round-trip time in seconds.
    pub fn rtt(&self) -> f64 {
        self.clone_handle.rtt().as_secs_f64()
    }
}

/// Pair returned by [`Session::open_bi`] / [`Session::accept_bi`].
#[derive(uniffi::Record)]
pub struct BiStream {
    pub send: Arc<SendStream>,
    pub recv: Arc<RecvStream>,
}

/// IP address + port of a remote peer.
#[derive(Debug, Clone, uniffi::Record)]
pub struct RemoteAddress {
    pub host: String,
    pub port: u16,
}
