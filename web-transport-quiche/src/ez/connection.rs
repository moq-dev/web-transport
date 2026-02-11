use std::sync::Arc;
use std::{
    future::poll_fn,
    ops::Deref,
    sync::Mutex,
    task::{Poll, Waker},
};
use thiserror::Error;
use tokio_quiche::quiche;

use crate::ez::DriverState;

use super::{Lock, RecvStream, SendStream};

/// An errors returned by [Connection].
#[derive(Clone, Error, Debug)]
pub enum ConnectionError {
    #[error("quiche error: {0}")]
    Quiche(#[from] quiche::Error),

    #[error("remote CONNECTION_CLOSE: code={0} reason={1}")]
    Remote(u64, String),

    #[error("local CONNECTION_CLOSE: code={0} reason={1}")]
    Local(u64, String),

    /// All Connection references were dropped without an explicit close.
    #[error("connection dropped")]
    Dropped,

    /// An unknown error occurred in tokio-quiche.
    #[error("unknown error: {0}")]
    Unknown(String),
}

#[derive(Default)]
struct ConnectionClosedState {
    err: Option<ConnectionError>,
    wakers: Vec<Waker>,
}

#[derive(Clone, Default)]
pub(super) struct ConnectionClosed {
    state: Arc<Mutex<ConnectionClosedState>>,
}

impl ConnectionClosed {
    pub fn abort(&self, err: ConnectionError) -> Vec<Waker> {
        let mut state = self.state.lock().unwrap();
        if state.err.is_some() {
            return Vec::new();
        }

        state.err = Some(err);
        std::mem::take(&mut state.wakers)
    }

    // Blocks until the connection is closed and drained.
    pub fn poll(&self, waker: &Waker) -> Poll<ConnectionError> {
        let mut state = self.state.lock().unwrap();
        if state.err.is_some() {
            return Poll::Ready(state.err.clone().unwrap());
        }

        state.wakers.push(waker.clone());

        Poll::Pending
    }

    pub fn is_closed(&self) -> bool {
        self.state.lock().unwrap().err.is_some()
    }
}

// Closes the connection when all references are dropped.
struct ConnectionClose {
    driver: Lock<DriverState>,
}

impl ConnectionClose {
    pub fn new(driver: Lock<DriverState>) -> Self {
        Self { driver }
    }

    pub fn close(&self, err: ConnectionError) {
        let wakers = self.driver.lock().close(err);

        for waker in wakers {
            waker.wake();
        }
    }

    pub async fn wait(&self) -> ConnectionError {
        poll_fn(|cx| self.driver.lock().closed(cx.waker())).await
    }

    pub fn is_closed(&self) -> bool {
        self.driver.lock().is_closed()
    }
}

impl Drop for ConnectionClose {
    fn drop(&mut self) {
        self.close(ConnectionError::Dropped);
    }
}

/// A QUIC connection that can create and accept streams.
///
/// This is a handle to an established QUIC connection. It can be cloned to create
/// multiple handles to the same connection. The connection will be closed when all
/// handles are dropped.
#[derive(Clone)]
pub struct Connection {
    inner: Arc<tokio_quiche::QuicConnection>,

    // Unbounded
    accept_bi: flume::Receiver<(SendStream, RecvStream)>,
    accept_uni: flume::Receiver<RecvStream>,

    driver: Lock<DriverState>,

    // Held in an Arc so we can use Drop when all references are dropped.
    close: Arc<ConnectionClose>,
}

impl Connection {
    pub(super) fn new(
        conn: tokio_quiche::QuicConnection,
        driver: Lock<DriverState>,
        accept_bi: flume::Receiver<(SendStream, RecvStream)>,
        accept_uni: flume::Receiver<RecvStream>,
    ) -> Self {
        let close = Arc::new(ConnectionClose::new(driver.clone()));

        Self {
            inner: Arc::new(conn),
            accept_bi,
            accept_uni,
            driver,
            close,
        }
    }

    /// Accept a bidirectional stream created by the remote peer.
    pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), ConnectionError> {
        tokio::select! {
            Ok(res) = self.accept_bi.recv_async() => Ok(res),
            res = self.closed() => Err(res),
        }
    }

    /// Accept a unidirectional stream created by the remote peer.
    pub async fn accept_uni(&self) -> Result<RecvStream, ConnectionError> {
        tokio::select! {
            Ok(res) = self.accept_uni.recv_async() => Ok(res),
            res = self.closed() => Err(res),
        }
    }

    /// Open a new bidirectional stream.
    ///
    /// May block while there are too many concurrent streams.
    pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), ConnectionError> {
        let (wakeup, id, send, recv) = poll_fn(|cx| self.driver.lock().open_bi(cx.waker())).await?;
        if let Some(wakeup) = wakeup {
            wakeup.wake();
        }

        let send = SendStream::new(id, send, self.driver.clone());
        let recv = RecvStream::new(id, recv, self.driver.clone());

        Ok((send, recv))
    }

    /// Open a new unidirectional stream.
    ///
    /// May block while there are too many concurrent streams.
    pub async fn open_uni(&self) -> Result<SendStream, ConnectionError> {
        let (wakeup, id, send) = poll_fn(|cx| self.driver.lock().open_uni(cx.waker())).await?;
        if let Some(wakeup) = wakeup {
            wakeup.wake();
        }

        let send = SendStream::new(id, send, self.driver.clone());
        Ok(send)
    }

    /// Immediately close the connection with an error code and reason.
    ///
    /// **NOTE**: You should wait until [Connection::closed] returns to ensure the CONNECTION_CLOSE frame is sent.
    /// Otherwise, the close may be lost and the peer will have to wait for a timeout.
    pub fn close(&self, code: u64, reason: &str) {
        self.close
            .close(ConnectionError::Local(code, reason.to_string()));
    }

    /// Wait until the connection is closed (or acknowledged) by the remote, returning the error.
    pub async fn closed(&self) -> ConnectionError {
        self.close.wait().await
    }

    /// Returns true if the connection is closed by either side.
    ///
    /// **NOTE**: This includes local closures, unlike [Connection::closed].
    pub fn is_closed(&self) -> bool {
        self.close.is_closed()
    }

    /// Returns the negotiated ALPN protocol, if the handshake has completed.
    pub fn alpn(&self) -> Option<Vec<u8>> {
        self.driver.lock().alpn().map(|a| a.to_vec())
    }

    /// Returns the SNI server name from the TLS ClientHello, if the handshake has completed.
    pub fn server_name(&self) -> Option<String> {
        self.driver.lock().server_name().map(|s| s.to_string())
    }
}

impl Deref for Connection {
    type Target = tokio_quiche::QuicConnection;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
