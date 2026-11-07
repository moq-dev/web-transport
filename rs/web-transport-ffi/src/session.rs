//! WebTransport session wrapped for UniFFI.
//!
//! Wraps `web_transport_quinn::Session` in `Option` so [`Drop`] can release
//! quinn-owned resources inside the [`RUNTIME`] context (see moq ae506266
//! for the bug this avoids).

use std::sync::Arc;

use crate::error::{map_session_error, WebTransportError};
use crate::ffi::{spawn_abortable, RUNTIME};
use crate::recv_stream::RecvStream;
use crate::send_stream::SendStream;

/// One peer of a WebTransport session.
///
/// Keep this object alive while using its streams. Releasing the final Session
/// reference closes the connection even if stream objects remain. Calling close
/// first allows its bounded graceful close to finish after the Session is released.
///
/// All async methods drive their futures on the FFI runtime so the foreign
/// language can poll them without setting up a tokio context of its own.
/// Cancelling a call aborts its runtime task and releases its Session handle.
#[derive(uniffi::Object)]
pub struct Session {
    inner: Option<web_transport_quinn::Session>,
}

impl Session {
    pub fn new(session: web_transport_quinn::Session) -> Arc<Self> {
        Arc::new(Self {
            inner: Some(session),
        })
    }

    fn session(&self) -> &web_transport_quinn::Session {
        // Only Drop takes this field, and it has exclusive access to self.
        self.inner.as_ref().expect("Session is alive")
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _guard = RUNTIME.enter();
        self.inner.take();
    }
}

#[uniffi::export]
impl Session {
    /// Open a new bidirectional stream.
    pub async fn open_bi(&self) -> Result<BiStream, WebTransportError> {
        let session = self.session().clone();
        let (send, recv) =
            spawn_abortable(async move { session.open_bi().await.map_err(map_session_error) })
                .await?;
        Ok(BiStream {
            send: SendStream::new(send),
            recv: RecvStream::new(recv),
        })
    }

    /// Open a new unidirectional (send-only) stream.
    pub async fn open_uni(&self) -> Result<Arc<SendStream>, WebTransportError> {
        let session = self.session().clone();
        let send =
            spawn_abortable(async move { session.open_uni().await.map_err(map_session_error) })
                .await?;
        Ok(SendStream::new(send))
    }

    /// Accept the next bidirectional stream opened by the peer.
    pub async fn accept_bi(&self) -> Result<BiStream, WebTransportError> {
        let session = self.session().clone();
        let (send, recv) =
            spawn_abortable(async move { session.accept_bi().await.map_err(map_session_error) })
                .await?;
        Ok(BiStream {
            send: SendStream::new(send),
            recv: RecvStream::new(recv),
        })
    }

    /// Accept the next unidirectional stream opened by the peer.
    pub async fn accept_uni(&self) -> Result<Arc<RecvStream>, WebTransportError> {
        let session = self.session().clone();
        let recv =
            spawn_abortable(async move { session.accept_uni().await.map_err(map_session_error) })
                .await?;
        Ok(RecvStream::new(recv))
    }

    /// Send an unreliable datagram. Fails if the payload exceeds
    /// [`Self::max_datagram_size`].
    pub fn send_datagram(&self, data: Vec<u8>) -> Result<(), WebTransportError> {
        let _guard = RUNTIME.enter();
        self.session()
            .send_datagram(bytes::Bytes::from(data))
            .map_err(map_session_error)
    }

    /// Wait for and return the next incoming datagram.
    pub async fn receive_datagram(&self) -> Result<Vec<u8>, WebTransportError> {
        let session = self.session().clone();
        let data =
            spawn_abortable(
                async move { session.read_datagram().await.map_err(map_session_error) },
            )
            .await?;
        Ok(data.to_vec())
    }

    /// Close the session.
    #[uniffi::method(default(code = 0, reason = ""))]
    pub fn close(&self, code: u32, reason: String) {
        let _guard = RUNTIME.enter();
        self.session().close(code, reason.as_bytes());
    }

    /// Wait until the session is closed (for any reason).
    pub async fn wait_closed(&self) {
        let session = self.session().clone();
        let _ = spawn_abortable(async move {
            let _ = session.closed().await;
            Ok(())
        })
        .await;
    }

    /// Whether the session has been closed.
    pub fn is_closed(&self) -> bool {
        self.session().close_reason().is_some()
    }

    /// If the session has been closed, return the close error structured as
    /// a [`WebTransportError`]. Returns `None` while the session is alive.
    pub fn close_reason(&self) -> Option<WebTransportError> {
        self.session()
            .close_reason()
            .map(crate::error::map_session_error)
    }

    /// Maximum payload size accepted by [`Self::send_datagram`].
    pub fn max_datagram_size(&self) -> u64 {
        self.session().max_datagram_size() as u64
    }

    /// Remote peer address as a `(host, port)` tuple.
    pub fn remote_address(&self) -> RemoteAddress {
        let addr = self.session().remote_address();
        RemoteAddress {
            host: addr.ip().to_string(),
            port: addr.port(),
        }
    }

    /// Current estimated round-trip time in seconds.
    pub fn rtt(&self) -> f64 {
        self.session().rtt().as_secs_f64()
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
