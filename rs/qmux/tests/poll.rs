//! Drive a real qmux session entirely through the poll surface.
//!
//! Nothing here calls an `async` method on the transport. Every operation goes
//! through `web_transport_trait::poll`, stepped with `poll_fn` so the trait's own
//! `Poll` contract is what is under test — including that a parked operation is
//! actually *woken* (registrations survive an abandoned poll and losers of a race
//! are re-woken), which is the part that breaks if a waker is lost.

#![cfg(feature = "tcp")]

use std::future::{poll_fn, Future};
use std::time::Duration;

use qmux::{transport::Stream, Config, Session, Version};
use web_transport_trait::poll::{RecvStream as _, SendStream as _, Session as _};
use web_transport_trait::Error as _;

/// A client and server session over an in-memory duplex, already established.
async fn pair(configure: impl FnOnce(&mut Config)) -> (Session, Session) {
    let (a, b) = tokio::io::duplex(64 * 1024);
    let mut config = Config::new(Version::QMux02);
    configure(&mut config);

    let ta = Stream::new(a, config.version, config.max_record_size);
    let tb = Stream::new(b, config.version, config.max_record_size);
    let (client, server) = tokio::join!(
        Session::connect(ta, config.clone()),
        Session::accept(tb, config),
    );
    (client.unwrap(), server.unwrap())
}

/// Every test runs under this bound: a lost waker surfaces as a hang, and the
/// timeout converts it into a failure instead.
async fn bounded<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("operation hung: a waker was lost")
}

/// Open, write, finish, accept and read — all through `poll_*`.
#[tokio::test]
async fn a_uni_stream_round_trips_through_the_poll_surface() {
    let (mut client, mut server) = pair(|_| {}).await;

    let mut send = bounded(poll_fn(|cx| client.poll_open_uni(cx))).await.unwrap();
    let size = bounded(poll_fn(|cx| send.poll_write(cx, b"hello")))
        .await
        .unwrap();
    assert_eq!(size, 5);
    send.finish().unwrap();

    let mut recv = bounded(poll_fn(|cx| server.poll_accept_uni(cx)))
        .await
        .unwrap();

    let mut dst = [0u8; 16];
    let size = bounded(poll_fn(|cx| recv.poll_read(cx, &mut dst)))
        .await
        .unwrap()
        .expect("stream ended early");
    assert_eq!(&dst[..size], b"hello");

    // Drained and finished.
    assert_eq!(
        bounded(poll_fn(|cx| recv.poll_read(cx, &mut dst)))
            .await
            .unwrap(),
        None
    );
}

/// The chunked read hands out the buffered `Bytes` without an extra copy.
#[tokio::test]
async fn a_bi_stream_round_trips_through_the_poll_surface() {
    let (mut client, mut server) = pair(|_| {}).await;

    let (mut client_send, mut client_recv) =
        bounded(poll_fn(|cx| client.poll_open_bi(cx))).await.unwrap();
    bounded(poll_fn(|cx| client_send.poll_write(cx, b"ping")))
        .await
        .unwrap();

    let (mut server_send, mut server_recv) =
        bounded(poll_fn(|cx| server.poll_accept_bi(cx))).await.unwrap();

    let chunk = bounded(poll_fn(|cx| server_recv.poll_read_chunk(cx, 16)))
        .await
        .unwrap()
        .expect("stream ended early");
    assert_eq!(&chunk[..], b"ping");

    bounded(poll_fn(|cx| server_send.poll_write(cx, b"pong")))
        .await
        .unwrap();
    let chunk = bounded(poll_fn(|cx| client_recv.poll_read_chunk(cx, 16)))
        .await
        .unwrap()
        .expect("stream ended early");
    assert_eq!(&chunk[..], b"pong");
}

/// The datagram path, including that `poll_send_datagram` is retried with the
/// same payload rather than taking ownership.
#[tokio::test]
async fn datagrams_round_trip_through_the_poll_surface() {
    let (mut client, mut server) = pair(|_| {}).await;

    bounded(poll_fn(|cx| client.poll_send_datagram(cx, b"dgram")))
        .await
        .unwrap();

    let received = bounded(poll_fn(|cx| server.poll_recv_datagram(cx)))
        .await
        .unwrap();
    assert_eq!(&received[..], b"dgram");
}

/// A `Pending` operation must be resumed, not restarted: poll `poll_closed` once
/// against a waker that goes nowhere, then drive it to completion on a real one.
/// An implementation that lost its registration when the first poll was abandoned
/// would hang here.
#[tokio::test]
async fn a_pending_operation_is_resumed_not_restarted() {
    let (mut client, mut server) = pair(|_| {}).await;

    let mut closed = std::pin::pin!(poll_fn(|cx| client.poll_closed(cx)));

    // Register, then abandon that poll.
    let noop = std::task::Waker::noop();
    assert!(
        closed
            .as_mut()
            .poll(&mut std::task::Context::from_waker(noop))
            .is_pending(),
        "the session should still be open"
    );

    // Disambiguate from the async trait's `close`, which has the same name.
    web_transport_trait::poll::Session::close(&mut server, 42, "bye");

    let err = bounded(closed).await;
    assert_eq!(err.session_error(), Some((42, "bye".to_string())));
}

/// Two clones parked on the same accept must both be woken: kio wakes every
/// waiter and losers re-park, so the second stream reaches the second clone
/// rather than stranding it on a stolen registration.
#[tokio::test]
async fn racing_accepts_on_clones_are_both_woken() {
    let (client, server) = pair(|_| {}).await;

    let mut accepts = Vec::new();
    for _ in 0..2 {
        let mut clone = server.clone();
        accepts.push(tokio::spawn(async move {
            let mut recv = poll_fn(|cx| clone.poll_accept_uni(cx)).await.unwrap();
            poll_fn(|cx| recv.poll_read_chunk(cx, 16)).await.unwrap()
        }));
    }
    // Let both accepts park before any stream arrives.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = client;
    for payload in [b"one".as_slice(), b"two".as_slice()] {
        let mut send = bounded(poll_fn(|cx| client.poll_open_uni(cx))).await.unwrap();
        bounded(poll_fn(|cx| send.poll_write(cx, payload)))
            .await
            .unwrap();
        send.finish().unwrap();
    }

    let mut payloads = Vec::new();
    for accept in accepts {
        let chunk = bounded(accept).await.unwrap().expect("stream ended early");
        payloads.push(chunk);
    }
    payloads.sort();
    assert_eq!(payloads[0].as_ref(), b"one");
    assert_eq!(payloads[1].as_ref(), b"two");
}

/// A STOP_SENDING arriving while a write is parked on flow control must wake the
/// write: the poll registers with the stop signal alongside the credit it waits
/// on, and losing either registration would strand the writer forever.
#[tokio::test]
async fn a_stop_wakes_a_write_parked_on_flow_control() {
    let (mut client, mut server) = pair(|config| {
        // The peer's receive window for a uni stream we open: one write fits,
        // the next parks on stream credit.
        config.max_stream_data_uni = 4;
    })
    .await;

    let mut send = bounded(poll_fn(|cx| client.poll_open_uni(cx))).await.unwrap();
    let size = bounded(poll_fn(|cx| send.poll_write(cx, b"full")))
        .await
        .unwrap();
    assert_eq!(size, 4, "the first write should exhaust the stream window");

    // This write parks: the window is spent and the server isn't reading.
    let write = tokio::spawn(async move {
        let err = poll_fn(|cx| send.poll_write(cx, b"more"))
            .await
            .expect_err("the write should fail once the peer stops the stream");
        (send, err)
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!write.is_finished(), "the write should park on flow control");

    let mut recv = bounded(poll_fn(|cx| server.poll_accept_uni(cx)))
        .await
        .unwrap();
    recv.stop(7);

    let (mut send, err) = bounded(write).await.unwrap();
    assert_eq!(err.stream_error(), Some(7), "got {err:?}");

    // And `poll_closed` reports the same terminal state.
    let err = bounded(poll_fn(|cx| send.poll_closed(cx)))
        .await
        .expect_err("a stopped stream is closed with an error");
    assert_eq!(err.stream_error(), Some(7), "got {err:?}");
}
