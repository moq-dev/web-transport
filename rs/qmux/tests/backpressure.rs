//! Transport-backpressure behavior enabled by the split writer task:
//!
//!  - a write stalled on transport backpressure must NOT stall reads
//!    (head-of-line blocking across directions), and
//!  - unreliable datagrams must be *shed* the moment the transport backs up,
//!    rather than buffered behind a stalled socket where they'd arrive stale.

#![cfg(feature = "tcp")]

use std::time::Duration;

use bytes::Bytes;
use qmux::{Config, Error, Session, Transport, TransportReader, TransportWriter, Version};
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

impl TransportWriter for GatedWriter {
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

impl TransportReader for GatedReader {
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
        let _ = s.write(&vec![b'C'; 512 * 1024]).await; // blocks; may error on teardown
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
        let _ = s.write(&vec![b'F'; 512 * 1024]).await;
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

    assert!(
        received < N,
        "backpressured datagrams must be shed, but {received}/{N} arrived"
    );

    filler.abort();
}
