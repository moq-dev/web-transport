use bytes::Bytes;
use kio::{Waiter, WaiterList};
use rustls_pki_types::CertificateDer;
use std::sync::Arc;
use std::time::Duration;
use std::{
    ops::Deref,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    task::{ready, Poll},
};
use thiserror::Error;
use tokio_quiche::quiche;

use crate::ez::DriverState;

use super::{Lock, RecvStream, SendStream};

/// A point-in-time snapshot of QUIC connection statistics.
///
/// The driver refreshes this each event loop iteration once the connection is
/// established, so reads are cheap and lock-free of quiche internals.
#[derive(Clone, Copy, Debug, Default)]
pub struct ConnectionStats {
    /// Total bytes sent, including retransmissions and overhead.
    pub bytes_sent: u64,
    /// Total bytes received, including duplicates and overhead.
    pub bytes_received: u64,
    /// Total bytes detected as lost.
    pub bytes_lost: u64,
    /// Total QUIC packets sent.
    pub packets_sent: u64,
    /// Total QUIC packets received.
    pub packets_received: u64,
    /// Total QUIC packets detected as lost.
    pub packets_lost: u64,
    /// Smoothed round-trip time for the active path, if one is established.
    pub rtt: Option<Duration>,
    /// Estimated send rate in bits per second (from the congestion controller),
    /// if a path is established.
    pub send_rate: Option<u64>,
}

impl ConnectionStats {
    /// Snapshot the current stats from a live quiche connection.
    pub(super) fn from_quiche(conn: &tokio_quiche::quic::QuicheConnection) -> Self {
        let stats = conn.stats();
        // The first (active) path carries RTT and the delivery-rate estimate.
        let path = conn.path_stats().next();
        Self {
            bytes_sent: stats.sent_bytes,
            bytes_received: stats.recv_bytes,
            bytes_lost: stats.lost_bytes,
            packets_sent: stats.sent as u64,
            packets_received: stats.recv as u64,
            packets_lost: stats.lost as u64,
            rtt: path.as_ref().map(|p| p.rtt),
            // quiche reports the delivery rate in bytes/sec; the trait wants bits/sec.
            send_rate: path.as_ref().map(|p| p.delivery_rate.saturating_mul(8)),
        }
    }
}

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

    /// Everyone waiting to hear that the connection went away.
    ///
    /// Every blocked read, write, accept and handshake parks here, so this is the list
    /// that most needs weak registrations: a caller that gives up releases its slot
    /// instead of leaving a waker parked for the life of the connection.
    waiters: WaiterList,
}

#[derive(Clone, Default)]
pub(super) struct ConnectionClosed {
    state: Arc<Mutex<ConnectionClosedState>>,
}

impl ConnectionClosed {
    #[must_use = "wake the waiters"]
    pub fn abort(&self, err: ConnectionError) -> WaiterList {
        let mut state = self.state.lock().unwrap();
        if state.err.is_some() {
            return WaiterList::new();
        }

        state.err = Some(err);
        state.waiters.take()
    }

    // Blocks until the connection is closed and drained.
    pub fn poll(&self, waiter: &Waiter) -> Poll<ConnectionError> {
        let mut state = self.state.lock().unwrap();
        if state.err.is_some() {
            return Poll::Ready(state.err.clone().unwrap());
        }

        waiter.register(&mut state.waiters);

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
        // Wake outside the lock, as everywhere else in this module.
        let mut waiters = self.driver.lock().close(err);
        waiters.wake();
    }

    /// Only the test below still needs this: accept and datagram reads used to race
    /// it in a `select!`, and now poll the driver's own error state directly.
    #[cfg(test)]
    pub async fn error(&self) -> ConnectionError {
        kio::wait(|waiter| self.driver.lock().error(waiter)).await
    }

    pub async fn wait(&self) -> ConnectionError {
        kio::wait(|waiter| self.driver.lock().closed(waiter)).await
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
/// This is a handle to an *established* connection: it's only reachable through
/// [Incoming::accept](super::Incoming::accept) or
/// [Connecting::established](super::Connecting::established), both of which wait for
/// the TLS handshake. So anything the handshake settles — [alpn](Connection::alpn),
/// [server_name](Connection::server_name), [peer_certificates](Connection::peer_certificates) —
/// is already known here, and a `None` means the value is genuinely absent rather
/// than not yet available.
///
/// It can be cloned to create multiple handles to the same connection. The
/// connection will be closed when all handles are dropped.
#[derive(Clone)]
pub struct Connection {
    inner: Arc<tokio_quiche::QuicConnection>,

    // Accepted streams and inbound datagrams are queued in `DriverState`, not in a
    // channel, so they can be polled: a channel's receive future owns its waker
    // registration and drops it with the future.
    //
    // Outbound datagrams stay a channel — bounded, and dropping on full is
    // consistent with the unreliable QUIC datagram contract.
    dgram_out: flume::Sender<Bytes>,
    dgram_max: Arc<AtomicUsize>,

    driver: Lock<DriverState>,

    // Held in an Arc so we can use Drop when all references are dropped.
    close: Arc<ConnectionClose>,
}

impl Connection {
    pub(super) fn new(
        conn: tokio_quiche::QuicConnection,
        driver: Lock<DriverState>,
        dgram_out: flume::Sender<Bytes>,
        dgram_max: Arc<AtomicUsize>,
    ) -> Self {
        let close = Arc::new(ConnectionClose::new(driver.clone()));

        Self {
            inner: Arc::new(conn),
            dgram_out,
            dgram_max,
            driver,
            close,
        }
    }

    /// Accept a bidirectional stream created by the remote peer.
    pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), ConnectionError> {
        kio::wait(|waiter| self.poll_accept_bi(waiter)).await
    }

    /// Poll for a bidirectional stream created by the remote peer.
    pub fn poll_accept_bi(
        &self,
        waiter: &Waiter,
    ) -> Poll<Result<(SendStream, RecvStream), ConnectionError>> {
        let (id, send, recv) = ready!(self.driver.lock().poll_accept_bi(waiter))?;

        Poll::Ready(Ok((
            SendStream::new(id, send, self.driver.clone()),
            RecvStream::new(id, recv, self.driver.clone()),
        )))
    }

    /// Accept a unidirectional stream created by the remote peer.
    pub async fn accept_uni(&self) -> Result<RecvStream, ConnectionError> {
        kio::wait(|waiter| self.poll_accept_uni(waiter)).await
    }

    /// Poll for a unidirectional stream created by the remote peer.
    pub fn poll_accept_uni(&self, waiter: &Waiter) -> Poll<Result<RecvStream, ConnectionError>> {
        let (id, recv) = ready!(self.driver.lock().poll_accept_uni(waiter))?;
        Poll::Ready(Ok(RecvStream::new(id, recv, self.driver.clone())))
    }

    /// Open a new bidirectional stream.
    ///
    /// May block while there are too many concurrent streams.
    pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), ConnectionError> {
        let (wakeup, id, send, recv) =
            kio::wait(|waiter| self.driver.lock().open_bi(waiter)).await?;
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
        let (wakeup, id, send) = kio::wait(|waiter| self.driver.lock().open_uni(waiter)).await?;
        if let Some(wakeup) = wakeup {
            wakeup.wake();
        }

        let send = SendStream::new(id, send, self.driver.clone());
        Ok(send)
    }

    /// Poll to open a new unidirectional stream.
    pub fn poll_open_uni(&self, waiter: &Waiter) -> Poll<Result<SendStream, ConnectionError>> {
        let (wakeup, id, send) = ready!(self.driver.lock().open_uni(waiter))?;
        if let Some(wakeup) = wakeup {
            wakeup.wake();
        }

        Poll::Ready(Ok(SendStream::new(id, send, self.driver.clone())))
    }

    /// Poll to open a new bidirectional stream.
    pub fn poll_open_bi(
        &self,
        waiter: &Waiter,
    ) -> Poll<Result<(SendStream, RecvStream), ConnectionError>> {
        let (wakeup, id, send, recv) = ready!(self.driver.lock().open_bi(waiter))?;
        if let Some(wakeup) = wakeup {
            wakeup.wake();
        }

        Poll::Ready(Ok((
            SendStream::new(id, send, self.driver.clone()),
            RecvStream::new(id, recv, self.driver.clone()),
        )))
    }

    /// Poll until the connection is closed and drained.
    pub fn poll_closed(&self, waiter: &Waiter) -> Poll<ConnectionError> {
        self.driver.lock().closed(waiter)
    }

    /// Receive the next application datagram from the remote peer.
    ///
    /// Waits until a datagram arrives or the connection is closed.
    pub async fn read_datagram(&self) -> Result<Bytes, ConnectionError> {
        kio::wait(|waiter| self.poll_read_datagram(waiter)).await
    }

    /// Poll for the next application datagram from the remote peer.
    pub fn poll_read_datagram(&self, waiter: &Waiter) -> Poll<Result<Bytes, ConnectionError>> {
        self.driver.lock().poll_dgram_in(waiter)
    }

    /// Queue an application datagram for the driver to send.
    ///
    /// Datagrams are unreliable. If the outbound channel is full the datagram
    /// is **dropped** (returning `Ok(())`) — backpressure surfaces as packet
    /// loss, which matches the QUIC datagram contract. Returns
    /// `Err(ConnectionError::Dropped)` only when the driver itself is gone.
    pub fn send_datagram(&self, data: Bytes) -> Result<(), ConnectionError> {
        match self.dgram_out.try_send(data) {
            Ok(()) => {}
            Err(flume::TrySendError::Full(_)) => {
                tracing::trace!("dropping outbound datagram: channel full");
                return Ok(());
            }
            Err(flume::TrySendError::Disconnected(_)) => {
                return Err(ConnectionError::Dropped);
            }
        }

        // Nudge the driver so it picks up the new datagram on the next poll.
        let waker = self.driver.lock().wake();
        if let Some(w) = waker {
            w.wake();
        }
        Ok(())
    }

    /// Maximum size of a datagram that can be sent right now.
    ///
    /// Returns `None` when datagrams are disabled in the peer's transport parameters.
    pub fn max_datagram_size(&self) -> Option<usize> {
        let v = self.dgram_max.load(Ordering::Relaxed);
        if v == 0 {
            None
        } else {
            Some(v)
        }
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

    /// Returns the negotiated ALPN protocol, or `None` if the peers negotiated none.
    pub fn alpn(&self) -> Option<Vec<u8>> {
        self.driver.lock().alpn().map(|a| a.to_vec())
    }

    /// Returns the TLS server name, or `None` if the client sent no SNI.
    ///
    /// On a server this is the name the client asked for; on a client it's the name
    /// it sent, which is the dial host unless [ClientBuilder::with_server_name](super::ClientBuilder::with_server_name)
    /// overrode it.
    pub fn server_name(&self) -> Option<String> {
        self.driver.lock().server_name().map(|s| s.to_string())
    }

    /// Returns the peer's certificate chain, leaf first.
    ///
    /// The chain has already been verified according to the endpoint's
    /// configuration: [ClientAuth](super::ClientAuth) on a server, or the
    /// verification mode chosen on [ClientBuilder](super::ClientBuilder).
    ///
    /// A server returns `None` unless it requested a client certificate *and* the
    /// client presented one, so this doubles as the "was this peer authenticated"
    /// check under [ClientAuth::Optional](super::ClientAuth::Optional).
    pub fn peer_certificates(&self) -> Option<Vec<CertificateDer<'static>>> {
        self.driver.lock().peer_certificates().map(|c| c.to_vec())
    }

    /// Returns the most recent connection statistics snapshot.
    pub fn stats(&self) -> ConnectionStats {
        self.driver.lock().stats()
    }
}

impl Deref for Connection {
    type Target = tokio_quiche::QuicConnection;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[cfg(test)]
mod tests {
    use futures::FutureExt;

    use super::*;

    #[test]
    fn local_close_is_an_error_before_driver_is_closed() {
        let close = ConnectionClose::new(Lock::new(DriverState::new(false)));

        close.close(ConnectionError::Local(42, "done".to_string()));

        assert!(matches!(
            close.error().now_or_never(),
            Some(ConnectionError::Local(42, reason)) if reason == "done"
        ));
        assert!(close.wait().now_or_never().is_none());
    }
}
