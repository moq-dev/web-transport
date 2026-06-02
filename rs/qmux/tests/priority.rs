//! Integration tests for per-stream send prioritization.
//!
//! These drive a real `Session` over a `ThrottledTransport` whose `send`
//! sleeps a few milliseconds, so the session's bounded priority queue fills up
//! and the scheduler's ordering policy actually takes effect (when the socket
//! accepts everything instantly, prioritization is moot).

use std::time::Duration;

use bytes::Bytes;
use qmux::{Config, Error, Session, Transport, Version};
use tokio::sync::mpsc;
use web_transport_trait::{RecvStream as _, SendStream as _, Session as _};

/// An in-memory transport that relays whole messages between a connected pair,
/// adding a fixed per-`send` delay to create backpressure.
struct ThrottledTransport {
    tx: mpsc::Sender<Bytes>,
    rx: mpsc::Receiver<Bytes>,
    delay: Duration,
}

impl Transport for ThrottledTransport {
    async fn send(&mut self, data: Bytes) -> Result<(), Error> {
        // The delay is what fills the session's outbound priority queue: while
        // we're sleeping here, the writer loop can't pull the next frame, so
        // producers back up behind the capacity bound.
        tokio::time::sleep(self.delay).await;
        self.tx.send(data).await.map_err(|_| Error::Closed)
    }

    async fn recv(&mut self) -> Result<Bytes, Error> {
        self.rx.recv().await.ok_or(Error::Closed)
    }

    async fn close(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

/// Build a connected client/server session pair over throttled in-memory pipes.
fn pair(delay: Duration) -> (Session, Session) {
    let (c2s_tx, c2s_rx) = mpsc::channel(64);
    let (s2c_tx, s2c_rx) = mpsc::channel(64);

    let client_transport = ThrottledTransport {
        tx: c2s_tx,
        rx: s2c_rx,
        delay,
    };
    let server_transport = ThrottledTransport {
        tx: s2c_tx,
        rx: c2s_rx,
        delay,
    };

    let config = Config::new(Version::QMux01, None);
    let client = Session::connect(client_transport, config.clone());
    let server = Session::accept(server_transport, config);
    (client, server)
}

/// Higher-priority stream data jumps ahead of a low-priority backlog.
///
/// We write a large low-priority payload first (filling the queue), then a
/// small high-priority one. With prioritization, the high-priority stream's
/// FIN-terminated data fully arrives while the low-priority stream is still
/// trickling through.
#[tokio::test]
async fn higher_priority_completes_first() {
    let (client, server) = pair(Duration::from_millis(5));

    const LO_LEN: usize = 200 * 1024;
    const HI_LEN: usize = 4 * 1024;

    let writer = tokio::spawn(async move {
        let mut lo = client.open_uni().await.unwrap();
        let mut hi = client.open_uni().await.unwrap();
        lo.set_priority(10);
        hi.set_priority(200);

        // Fill the queue with low-priority data first.
        lo.write(&vec![b'L'; LO_LEN]).await.unwrap();
        lo.finish().unwrap();

        // Then enqueue the small high-priority payload.
        hi.write(&vec![b'H'; HI_LEN]).await.unwrap();
        hi.finish().unwrap();

        // Return the client so the session stays alive and the test can await
        // this task deterministically rather than detaching it.
        client
    });

    // The server accepts streams in arrival order: lo first, then hi.
    let lo = server.accept_uni().await.unwrap();
    let mut hi = server.accept_uni().await.unwrap();

    // Drain lo in the background while we read hi in the foreground. (We can't
    // probe lo with lo.closed(): qmux's recv closed() consumes inbound data,
    // which would corrupt the byte-count assertion below.)
    let lo_task = tokio::spawn(async move {
        let mut lo = lo;
        let mut total = 0usize;
        while let Some(chunk) = lo.read_chunk(64 * 1024).await.unwrap() {
            assert!(chunk.iter().all(|&b| b == b'L'));
            total += chunk.len();
        }
        total
    });

    let hi_data = hi.read_all().await.unwrap();
    assert_eq!(hi_data.len(), HI_LEN);
    assert!(hi_data.iter().all(|&b| b == b'H'));

    // The high-priority stream must finish while the low-priority backlog is
    // still in-flight — proof that it preempted, not merely that lo arrives.
    assert!(
        !lo_task.is_finished(),
        "low-priority stream should still be in-flight when the high-priority stream completes"
    );

    let lo_total = lo_task.await.unwrap();
    assert_eq!(
        lo_total, LO_LEN,
        "low-priority stream must still deliver all bytes"
    );

    let _client = tokio::time::timeout(Duration::from_secs(2), writer)
        .await
        .expect("writer task should complete")
        .expect("writer task panicked");
}

/// Equal-priority streams interleave (round-robin) rather than one draining
/// fully before the other starts.
#[tokio::test]
async fn equal_priority_interleaves() {
    let (client, server) = pair(Duration::from_millis(2));

    const LEN: usize = 128 * 1024;

    let writer = tokio::spawn(async move {
        let mut a = client.open_uni().await.unwrap();
        let mut b = client.open_uni().await.unwrap();
        a.set_priority(50);
        b.set_priority(50);

        // Both write bulk concurrently so frames from each are queued together.
        let wa = async {
            a.write(&vec![b'A'; LEN]).await.unwrap();
            a.finish().unwrap();
        };
        let wb = async {
            b.write(&vec![b'B'; LEN]).await.unwrap();
            b.finish().unwrap();
        };
        tokio::join!(wa, wb);

        client
    });

    let mut a = server.accept_uni().await.unwrap();
    let mut b = server.accept_uni().await.unwrap();

    // Read a's first chunk, then check that b has *also* started producing
    // before a is fully drained — i.e. they interleave.
    let first_a = a.read_chunk(8 * 1024).await.unwrap().unwrap();
    assert!(!first_a.is_empty());

    let first_b = b.read_chunk(8 * 1024).await.unwrap().unwrap();
    assert!(
        !first_b.is_empty(),
        "second equal-priority stream must start before the first drains"
    );

    // Finish draining both for completeness.
    let mut a_total = first_a.len();
    while let Some(c) = a.read_chunk(64 * 1024).await.unwrap() {
        a_total += c.len();
    }
    let mut b_total = first_b.len();
    while let Some(c) = b.read_chunk(64 * 1024).await.unwrap() {
        b_total += c.len();
    }
    assert_eq!(a_total, LEN);
    assert_eq!(b_total, LEN);

    let _client = tokio::time::timeout(Duration::from_secs(2), writer)
        .await
        .expect("writer task should complete")
        .expect("writer task panicked");
}

/// A control frame (RESET_STREAM, via `reset()`) reaches the peer ahead of a
/// large data backlog queued on another stream.
#[tokio::test]
async fn control_precedes_data_backlog() {
    let (client, server) = pair(Duration::from_millis(5));

    const LO_LEN: usize = 200 * 1024;

    let mut bulk = client.open_uni().await.unwrap();
    let mut signal = client.open_uni().await.unwrap();
    bulk.set_priority(10);
    signal.write(b"x").await.unwrap();

    // Backlog of bulk data, written concurrently so the queue stays saturated
    // while we issue the reset below. The writer keeps `client` alive (and thus
    // the session) until we abort it during cleanup.
    let bulk_writer = tokio::spawn(async move {
        bulk.write(&vec![b'B'; LO_LEN]).await.ok();
        client.closed().await;
    });

    // Let the bulk backlog build up so the data queue is full.
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Now reset `signal`. Its RESET_STREAM goes through the control lane and
    // must preempt the bulk backlog.
    signal.reset(7);

    // `signal`'s `b"x"` is written before the bulk backlog, so the server
    // accepts it first.
    let mut signal_recv = server.accept_uni().await.unwrap();

    // The reset should surface quickly even though `bulk` has a big backlog.
    // `closed()` returns once the RESET_STREAM is observed.
    let reset_result = tokio::time::timeout(Duration::from_millis(500), signal_recv.closed()).await;
    assert!(
        reset_result.is_ok(),
        "RESET_STREAM should arrive ahead of the data backlog"
    );
    assert!(matches!(reset_result.unwrap(), Err(Error::StreamReset(_))));

    bulk_writer.abort();
}

/// Raising a stream's priority mid-stream must not corrupt its byte sequence:
/// the receiver reassembles exactly what was written, in order.
#[tokio::test]
async fn mid_stream_set_priority_preserves_order() {
    let (client, server) = pair(Duration::from_millis(2));

    // A distinctive, position-encoded payload so any reorder/loss is detectable.
    let payload: Vec<u8> = (0..200 * 1024).map(|i| (i % 251) as u8).collect();
    let expected = payload.clone();

    let writer = tokio::spawn(async move {
        // A competing low-priority bulk stream so there's a backlog to reorder against.
        let mut filler = client.open_uni().await.unwrap();
        filler.set_priority(1);
        let mut s = client.open_uni().await.unwrap();
        s.set_priority(5);

        filler.write(&vec![b'F'; 200 * 1024]).await.unwrap();
        filler.finish().unwrap();

        // Write the first half at low priority, bump priority, write the rest.
        let mid = payload.len() / 2;
        s.write(&payload[..mid]).await.unwrap();
        s.set_priority(250); // promote mid-stream
        s.write(&payload[mid..]).await.unwrap();
        s.finish().unwrap();

        client
    });

    let mut filler = server.accept_uni().await.unwrap();
    let mut s = server.accept_uni().await.unwrap();

    let got = s.read_all().await.unwrap();
    assert_eq!(
        got.as_ref(),
        expected.as_slice(),
        "byte sequence must be intact and in order"
    );

    let _ = filler.read_all().await;

    let _client = tokio::time::timeout(Duration::from_secs(2), writer)
        .await
        .expect("writer task should complete")
        .expect("writer task panicked");
}

/// Tearing down the session unblocks a producer parked on a full queue: the
/// pending `write` returns `Error::Closed`.
#[tokio::test]
async fn teardown_unblocks_blocked_writer() {
    // Long delay so the queue stays full and `write` blocks.
    let (client, server) = pair(Duration::from_millis(500));

    let mut s = client.open_uni().await.unwrap();

    // Spawn a writer that will fill the queue and then block.
    let handle = tokio::spawn(async move {
        // Far more than the 8-frame queue capacity worth of data.
        s.write(&vec![b'Z'; 1024 * 1024]).await
    });

    // Give it a moment to fill the queue and park.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !handle.is_finished(),
        "writer should be blocked on a full queue"
    );

    // Tear down the session.
    drop(client);
    drop(server);

    let result = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("writer should unblock on teardown")
        .unwrap();
    assert!(matches!(result, Err(Error::Closed)), "got {result:?}");
}
