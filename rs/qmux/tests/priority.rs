//! Integration tests for per-stream send prioritization.
//!
//! These drive a real `Session` over a `ThrottledTransport` whose `send`
//! sleeps a few milliseconds and can be metered, so the session's bounded
//! priority queue fills up and the scheduler's ordering policy actually takes
//! effect (when the socket accepts everything instantly, prioritization is moot).
//! Time is paused, so the throttle is virtual: it orders events
//! deterministically and costs no wall time.

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use bytes::Bytes;
use qmux::transport::{Reader, Transport, Writer};
use qmux::{Config, Error, Session, Version};
use tokio::sync::{mpsc, Semaphore};
use web_transport_trait::{RecvStream as _, SendStream as _, Session as _};

/// An in-memory transport that relays whole messages between a connected pair,
/// adding a fixed per-`send` delay to create backpressure. Each `send` also takes
/// a permit from `meter`, so a test can release records one at a time.
struct ThrottledTransport {
    tx: mpsc::Sender<Bytes>,
    rx: mpsc::Receiver<Bytes>,
    delay: Duration,
    meter: Arc<Semaphore>,
}

/// The throttled send half.
struct ThrottledWriter {
    tx: mpsc::Sender<Bytes>,
    delay: Duration,
    meter: Arc<Semaphore>,
}

/// The receive half.
struct ThrottledReader {
    rx: mpsc::Receiver<Bytes>,
}

impl Transport for ThrottledTransport {
    type Writer = ThrottledWriter;
    type Reader = ThrottledReader;

    fn split(self) -> (ThrottledWriter, ThrottledReader) {
        (
            ThrottledWriter {
                tx: self.tx,
                delay: self.delay,
                meter: self.meter,
            },
            ThrottledReader { rx: self.rx },
        )
    }
}

impl Writer for ThrottledWriter {
    async fn send(&mut self, data: Bytes) -> Result<(), Error> {
        // The delay is what fills the session's outbound priority queue: while
        // we're sleeping here, the writer task can't pull the next frame, so
        // producers back up behind the capacity bound.
        tokio::time::sleep(self.delay).await;
        self.meter
            .acquire()
            .await
            .map_err(|_| Error::Closed)?
            .forget();
        self.tx.send(data).await.map_err(|_| Error::Closed)
    }

    async fn close(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

impl Reader for ThrottledReader {
    async fn recv(&mut self) -> Result<Bytes, Error> {
        self.rx.recv().await.ok_or(Error::Closed)
    }
}

/// Build a connected client/server session pair over throttled in-memory pipes.
async fn pair(delay: Duration) -> (Session, Session) {
    let (client, server, _meter) = metered_pair(delay).await;
    (client, server)
}

/// Permits that never run out in these tests, and fit `Semaphore::MAX_PERMITS` on every target.
const UNLIMITED: usize = 1 << 20;

/// Like [`pair`], plus the meter on the client's sends, initially unlimited.
async fn metered_pair(delay: Duration) -> (Session, Session, Arc<Semaphore>) {
    let (c2s_tx, c2s_rx) = mpsc::channel(64);
    let (s2c_tx, s2c_rx) = mpsc::channel(64);
    let meter = Arc::new(Semaphore::new(UNLIMITED));

    let client_transport = ThrottledTransport {
        tx: c2s_tx,
        rx: s2c_rx,
        delay,
        meter: meter.clone(),
    };
    let server_transport = ThrottledTransport {
        tx: s2c_tx,
        rx: c2s_rx,
        delay,
        meter: Arc::new(Semaphore::new(UNLIMITED)),
    };

    let config = Config::new(Version::QMux01);
    // `connect`/`accept` await establishment, so drive both ends concurrently —
    // each side's handshake needs the other's transport parameters.
    let (client, server) = tokio::join!(
        Session::connect(client_transport, config.clone()),
        Session::accept(server_transport, config),
    );
    (client.unwrap(), server.unwrap(), meter)
}

/// Stop the metered side from sending anything more until permits are added.
async fn stop(meter: &Semaphore) {
    let available = meter.available_permits() as u32;
    meter.acquire_many(available).await.unwrap().forget();
}

/// Run every task until idle: paused time only advances once nothing can make
/// progress.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(1)).await;
}

/// Higher-priority stream data jumps ahead of a low-priority backlog.
///
/// With the transport stopped, a low-priority stream queues three records and
/// then a high-priority one queues three (two writes and its FIN), all within the
/// scheduler's capacity. Records are then released one at a time: the whole
/// high-priority stream must reach the peer before a second low-priority record.
/// Round-robin among equal priorities would interleave them instead.
#[tokio::test(start_paused = true)]
async fn higher_priority_completes_first() {
    const CHUNK: usize = 1000;

    let (client, server, meter) = metered_pair(Duration::ZERO).await;
    let mut lo = client.open_uni().await.unwrap();
    let mut hi = client.open_uni().await.unwrap();
    lo.set_priority(10);
    hi.set_priority(200);

    stop(&meter).await;
    for _ in 0..3 {
        lo.write(&[b'L'; CHUNK]).await.unwrap();
    }
    for _ in 0..2 {
        hi.write(&[b'H'; CHUNK]).await.unwrap();
    }
    hi.finish().unwrap();

    // Tally what the peer has received, per stream, as it arrives.
    let lo_bytes = Arc::new(AtomicUsize::new(0));
    let hi_done = Arc::new(AtomicBool::new(false));
    let reader = tokio::spawn({
        let (lo_bytes, hi_done) = (lo_bytes.clone(), hi_done.clone());
        async move {
            let mut readers = Vec::new();
            for _ in 0..2 {
                let mut recv = server.accept_uni().await.unwrap();
                let (lo_bytes, hi_done) = (lo_bytes.clone(), hi_done.clone());
                readers.push(tokio::spawn(async move {
                    let mut is_hi = false;
                    while let Some(chunk) = recv.read_chunk(usize::MAX).await.unwrap() {
                        if chunk.iter().all(|&b| b == b'L') {
                            lo_bytes.fetch_add(chunk.len(), Ordering::SeqCst);
                        } else {
                            assert!(chunk.iter().all(|&b| b == b'H'));
                            is_hi = true;
                        }
                    }
                    if is_hi {
                        hi_done.store(true, Ordering::SeqCst);
                    }
                }));
            }
            (server, readers)
        }
    });

    for _ in 0..6 {
        if hi_done.load(Ordering::SeqCst) {
            break;
        }
        meter.add_permits(1);
        settle().await;
    }
    assert!(
        hi_done.load(Ordering::SeqCst),
        "the high-priority stream never finished"
    );
    assert!(
        lo_bytes.load(Ordering::SeqCst) <= CHUNK,
        "{} low-priority bytes reached the peer before the high-priority stream finished",
        lo_bytes.load(Ordering::SeqCst)
    );

    // The low-priority stream still delivers everything.
    meter.add_permits(UNLIMITED);
    lo.finish().unwrap();
    let (_server, readers) = reader.await.unwrap();
    for reader in readers {
        reader.await.unwrap();
    }
    assert_eq!(lo_bytes.load(Ordering::SeqCst), 3 * CHUNK);
}

/// Equal-priority streams interleave (round-robin) rather than one draining
/// fully before the other starts.
#[tokio::test(start_paused = true)]
async fn equal_priority_interleaves() {
    let (client, server) = pair(Duration::from_millis(2)).await;

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
/// data backlog queued on another stream.
///
/// With the transport stopped, a bulk stream queues three records and then
/// another stream is reset. Releasing records one at a time, the reset must
/// arrive before a second bulk record.
#[tokio::test(start_paused = true)]
async fn control_precedes_data_backlog() {
    const CHUNK: usize = 1000;

    let (client, server, meter) = metered_pair(Duration::ZERO).await;
    let mut bulk = client.open_uni().await.unwrap();
    let mut signal = client.open_uni().await.unwrap();
    bulk.set_priority(10);

    // `signal` reaches the peer before the transport stops.
    signal.write(b"x").await.unwrap();
    let mut signal_recv = server.accept_uni().await.unwrap();
    let reset = tokio::spawn(async move { signal_recv.closed().await });

    stop(&meter).await;
    for _ in 0..3 {
        bulk.write(&[b'B'; CHUNK]).await.unwrap();
    }
    signal.reset(7);

    let bulk_bytes = Arc::new(AtomicUsize::new(0));
    let reader = tokio::spawn({
        let bulk_bytes = bulk_bytes.clone();
        async move {
            let mut recv = server.accept_uni().await.unwrap();
            while let Some(chunk) = recv.read_chunk(usize::MAX).await.unwrap() {
                bulk_bytes.fetch_add(chunk.len(), Ordering::SeqCst);
            }
            server
        }
    });

    for _ in 0..4 {
        if reset.is_finished() {
            break;
        }
        meter.add_permits(1);
        settle().await;
    }
    assert!(reset.is_finished(), "the RESET_STREAM never arrived");
    assert!(matches!(reset.await.unwrap(), Err(Error::StreamReset(_))));
    assert!(
        bulk_bytes.load(Ordering::SeqCst) <= CHUNK,
        "{} bulk bytes reached the peer ahead of the RESET_STREAM",
        bulk_bytes.load(Ordering::SeqCst)
    );

    // The backlog still drains in full.
    meter.add_permits(UNLIMITED);
    bulk.finish().unwrap();
    let _server = reader.await.unwrap();
    assert_eq!(bulk_bytes.load(Ordering::SeqCst), 3 * CHUNK);
}

/// Raising a stream's priority mid-stream must not corrupt its byte sequence:
/// the receiver reassembles exactly what was written, in order.
#[tokio::test(start_paused = true)]
async fn mid_stream_set_priority_preserves_order() {
    let (client, server) = pair(Duration::from_millis(2)).await;

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
#[tokio::test(start_paused = true)]
async fn teardown_unblocks_blocked_writer() {
    // Long delay so the queue stays full and `write` blocks.
    let (client, server) = pair(Duration::from_millis(500)).await;

    let mut s = client.open_uni().await.unwrap();

    // Spawn a writer that will fill the queue and then block.
    let handle = tokio::spawn(async move {
        // Far more than the 8-frame queue capacity worth of data.
        s.write(&vec![b'Z'; 1024 * 1024]).await
    });

    // On paused time this runs every task until idle, so the writer has filled
    // the queue and parked behind the throttled send.
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
