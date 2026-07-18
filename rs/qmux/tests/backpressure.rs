//! Transport-backpressure behavior enabled by the split writer task:
//!
//!  - a write stalled on transport backpressure must NOT stall reads
//!    (head-of-line blocking across directions), and
//!  - unreliable datagrams must be *shed* the moment the transport backs up,
//!    rather than buffered behind a stalled socket where they'd arrive stale.

#![cfg(feature = "tcp")]

use std::time::Duration;

use bytes::Bytes;
use qmux::transport::{Reader, Writer};
use qmux::{Config, Error, Session, Transport, Version};
use tokio::sync::{mpsc, watch};
use web_transport_trait::{RecvStream as _, SendStream as _, Session as _};

/// In-memory transport whose writer blocks while a shared gate is closed,
/// standing in for a socket that has stopped accepting writes. `gate: None`
/// means "always open" (no backpressure).
struct GatedTransport {
    tx: mpsc::Sender<Bytes>,
    rx: mpsc::Receiver<Bytes>,
    gate: Option<watch::Receiver<bool>>,
}

struct GatedWriter {
    tx: mpsc::Sender<Bytes>,
    gate: Option<watch::Receiver<bool>>,
}

struct GatedReader {
    rx: mpsc::Receiver<Bytes>,
}

impl Transport for GatedTransport {
    type Writer = GatedWriter;
    type Reader = GatedReader;

    fn split(self) -> (GatedWriter, GatedReader) {
        (
            GatedWriter {
                tx: self.tx,
                gate: self.gate,
            },
            GatedReader { rx: self.rx },
        )
    }
}

impl Writer for GatedWriter {
    async fn send(&mut self, data: Bytes) -> Result<(), Error> {
        // Block while the gate is closed — the test's stand-in for a full socket.
        if let Some(gate) = &mut self.gate {
            gate.wait_for(|&open| open)
                .await
                .map_err(|_| Error::Closed)?;
        }
        self.tx.send(data).await.map_err(|_| Error::Closed)
    }

    async fn close(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

impl Reader for GatedReader {
    async fn recv(&mut self) -> Result<Bytes, Error> {
        self.rx.recv().await.ok_or(Error::Closed)
    }
}

/// A client/server pair where only the *client's* writer is gated by `gate`.
/// The gate is open during construction so the handshake completes.
async fn pair(gate: watch::Receiver<bool>) -> (Session, Session) {
    let (c2s_tx, c2s_rx) = mpsc::channel(256);
    let (s2c_tx, s2c_rx) = mpsc::channel(256);

    let client_transport = GatedTransport {
        tx: c2s_tx,
        rx: s2c_rx,
        gate: Some(gate),
    };
    let server_transport = GatedTransport {
        tx: s2c_tx,
        rx: c2s_rx,
        gate: None,
    };

    let config = Config::new(Version::QMux01);
    let (client, server) = tokio::join!(
        Session::connect(client_transport, config.clone()),
        Session::accept(server_transport, config),
    );
    (client.unwrap(), server.unwrap())
}

/// The core head-of-line-blocking fix: with the client's writer wedged on
/// transport backpressure, the client must still receive data from the server.
/// Under the old single-task design the blocked `send().await` also froze the
/// reader, so this read would hang.
#[tokio::test]
async fn reads_are_not_blocked_by_a_stalled_writer() {
    let (gate_tx, gate_rx) = watch::channel(true); // open: handshake flows
    let (client, server) = pair(gate_rx).await;

    // Close the gate: the client's writer now blocks on its next send.
    gate_tx.send(false).unwrap();

    // The client fills its outbound pipeline with a large payload; the writer
    // task wedges on the gate with the queue full.
    let client_writer = client.clone();
    let writer = tokio::spawn(async move {
        let mut s = client_writer.open_uni().await.unwrap();
        let _ = s.write_all(&vec![b'C'; 512 * 1024]).await; // blocks; may error on teardown
        client_writer // keep the session alive
    });

    // Give the writer time to fill the pipeline and park on the gate.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !writer.is_finished(),
        "client writer should be parked on the closed gate"
    );

    // The server (ungated) sends a stream to the client.
    let mut ss = server.open_uni().await.unwrap();
    ss.write(b"hello from server").await.unwrap();
    ss.finish().unwrap();

    // The client must accept and read it despite its own writer being wedged.
    let mut rs = tokio::time::timeout(Duration::from_secs(2), client.accept_uni())
        .await
        .expect("accept must not be blocked by the stalled writer")
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(2), rs.read_all())
        .await
        .expect("read must not be blocked by the stalled writer")
        .unwrap();
    assert_eq!(&got[..], b"hello from server");

    // Reopen the gate so the writer can unblock, then clean up.
    gate_tx.send(true).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
}

/// Datagrams enqueued while the transport is backpressured are shed, not
/// buffered: after releasing the writer, the peer sees far fewer than were sent.
#[tokio::test]
async fn datagrams_are_shed_under_backpressure() {
    let (gate_tx, gate_rx) = watch::channel(true);
    let (client, server) = pair(gate_rx).await;

    assert!(
        client.max_datagram_size() > 0,
        "datagrams must be enabled for this test"
    );

    // Wedge the client writer, then fill the pipeline with stream data so the
    // writer channel is provably at zero free capacity.
    gate_tx.send(false).unwrap();
    let client_filler = client.clone();
    let filler = tokio::spawn(async move {
        let mut s = client_filler.open_uni().await.unwrap();
        let _ = s.write_all(&vec![b'F'; 512 * 1024]).await;
        client_filler
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Every datagram in this burst should be shed: the writer is backpressured.
    const N: usize = 200;
    for i in 0..N {
        client
            .send_datagram(Bytes::from(vec![i as u8; 8]))
            .expect("send_datagram is best-effort and must not error when backpressured");
    }

    // Release the writer and count what actually reaches the server.
    gate_tx.send(true).unwrap();
    let mut received = 0usize;
    while let Ok(Ok(_)) =
        tokio::time::timeout(Duration::from_millis(200), server.recv_datagram()).await
    {
        received += 1;
    }

    // The outbound datagram lane is 64 deep (DATAGRAM_SEND_BUFFER), so at most
    // ~64 of the 200 can sit buffered while the writer is wedged; the rest are
    // shed. Bound the count well under N so a regression that grows the lane or
    // stops shedding is caught — `received < N` alone would still pass with, say,
    // a 128-deep buffer.
    assert!(
        received <= 96,
        "backpressured datagrams must be shed down to ~the 64-deep lane, but {received}/{N} arrived"
    );

    filler.abort();
}

/// A connection whose writer is wedged on transport backpressure DEFERS its idle
/// timeout — the wedge proves the peer's receive window is full, so it's alive and
/// we simply can't get a keep-alive out. But the deferral is *bounded*: a peer that
/// dies with our send buffer full is still reclaimed within roughly one extra idle
/// window, instead of hanging until the transport's own (much longer) timeout.
#[tokio::test]
async fn idle_timeout_deferred_but_bounded_under_backpressure() {
    let (c2s_tx, c2s_rx) = mpsc::channel(256);
    let (s2c_tx, s2c_rx) = mpsc::channel(256);
    // Keep the client's receive channel open even after the server task goes
    // away, so the client sees a *silent but open* peer rather than a transport
    // close. This isolates the client's own idle logic after both transport
    // writers are gated: otherwise the server's successful QX_PING writes would
    // count as activity and correctly keep the client alive.
    let _s2c_keepalive = s2c_tx.clone();

    let (gate_tx, gate_rx) = watch::channel(true); // open: handshake flows
    let client_transport = GatedTransport {
        tx: c2s_tx,
        rx: s2c_rx,
        gate: Some(gate_rx.clone()),
    };
    let server_transport = GatedTransport {
        tx: s2c_tx,
        rx: c2s_rx,
        gate: Some(gate_rx),
    };

    // Short idle timeout so the test is quick; the deferral grace is one more
    // window, so a stuck-backpressured peer is reclaimed at roughly 2× (~300ms).
    let mut config = Config::new(Version::QMux01);
    config.max_idle_timeout = 150;
    let (client, server) = tokio::join!(
        Session::connect(client_transport, config.clone()),
        Session::accept(server_transport, config),
    );
    let client = client.unwrap();
    let _server = server.unwrap();

    // Wedge the client writer: close the gate, then push a write it can't drain.
    gate_tx.send(false).unwrap();
    let client_writer = client.clone();
    let writer = tokio::spawn(async move {
        let mut s = client_writer.open_uni().await.unwrap();
        let _ = s.write(&vec![b'X'; 256 * 1024]).await;
        client_writer
    });

    // Observe the client's close reason (resolves only once it actually closes).
    let client_closed = client.clone();
    let closed = tokio::spawn(async move { client_closed.closed().await });

    // Past the raw 150ms idle window but within the bounded grace: still open.
    // With no deferral the client would already have idle-closed by now.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !closed.is_finished(),
        "idle-close must be deferred while the writer is backpressured (within grace)"
    );

    // Past the grace: the connection is reclaimed even though still backpressured,
    // so a peer that died under backpressure can't hang here indefinitely.
    let reason = tokio::time::timeout(Duration::from_millis(600), closed)
        .await
        .expect("bounded deferral must eventually idle-close a stuck-backpressured peer")
        .unwrap();
    assert!(
        matches!(reason, Error::IdleTimeout),
        "expected IdleTimeout once the grace elapses, got {reason:?}"
    );

    writer.abort();
}
