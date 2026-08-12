//! A browser harness for the poll state machine.
//!
//! `cargo check` proves this crate compiles for `wasm32`; nothing in CI runs it,
//! because the browser API it wraps only exists in a browser. So the paths that are
//! easiest to get wrong — an accept whose handle went away, a closed-watch racing
//! buffered bytes, a peer reset code — had no coverage at all. This runs them
//! against a real browser and a real QUIC peer.
//!
//! Pair it with `harness-server` from `web-transport-quinn`, which is driven by the
//! commands sent here. See `just harness`.
//!
//! Each check returns a row rather than panicking, so one failure does not hide the
//! rest.

use std::{
    rc::Rc,
    task::{Context, Poll, Waker},
};

use js_sys::{Array, Object, Reflect};
use wasm_bindgen::prelude::*;
use web_transport_wasm::{ClientBuilder, Error, RecvStream, Session};

/// Run every check against `url`, trusting the sha-256 certificate hash `hash`.
///
/// Returns an array of `{ name, ok, detail }` for the page to render.
#[wasm_bindgen]
pub async fn run(url: String, hash: String) -> Result<Array, JsValue> {
    let results = Array::new();

    let hash = decode_hex(&hash).ok_or_else(|| JsValue::from_str("bad certificate hash"))?;
    let url: url::Url = url
        .parse()
        .map_err(|_| JsValue::from_str("bad harness url"))?;

    // Each check gets its own session, so one that closes or wedges the connection
    // cannot decide the outcome of the next.
    macro_rules! check {
        ($name:expr, $body:expr) => {{
            let outcome = match connect(&url, &hash).await {
                Ok(session) => match $body(session).await {
                    Ok(detail) => row($name, true, &detail),
                    Err(err) => row($name, false, &format!("{err}")),
                },
                Err(err) => row($name, false, &format!("connect failed: {err}")),
            };
            results.push(&outcome);
        }};
    }

    check!("echo round trip", echo_round_trip);
    check!("accept_uni delivers every stream", accept_all);
    check!("a dropped clone does not swallow a stream", orphaned_accept);
    check!(
        "closed() resolves with bytes still buffered",
        closed_with_buffered
    );
    check!("peer reset code reaches the receiver", peer_reset_code);
    check!("datagram round trip", datagram_round_trip);
    check!("session close code and reason survive", session_close);

    Ok(results)
}

async fn connect(url: &url::Url, hash: &[u8]) -> Result<Session, Error> {
    ClientBuilder::new()
        .with_server_certificate_hashes(vec![hash.to_vec()])
        .connect(url.clone())
        .await
}

/// Send a harness command and hand back the stream the reply arrives on.
async fn command(session: &Session, command: &str) -> Result<RecvStream, Error> {
    let (mut send, recv) = session.open_bi().await?;
    send.write(command.as_bytes()).await?;
    send.finish()?;
    Ok(recv)
}

/// Read a receive stream to the end.
async fn read_all(recv: &mut RecvStream) -> Result<String, Error> {
    let mut out = Vec::new();
    while let Some(chunk) = recv.read(4096).await? {
        out.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Open a stream, say something, and get it back. The baseline: if this fails,
/// nothing below means anything.
async fn echo_round_trip(session: Session) -> Result<String, Error> {
    let mut recv = command(&session, "echo hello harness").await?;
    let got = read_all(&mut recv).await?;

    expect(got == "hello harness", format!("echoed {got:?}"))
}

/// Every stream the peer opens is delivered, in order.
async fn accept_all(session: Session) -> Result<String, Error> {
    let mut ack = command(&session, "streams 3").await?;
    read_all(&mut ack).await?;

    let mut seen = Vec::new();
    for _ in 0..3 {
        let mut recv = session.accept_uni().await?;
        seen.push(read_all(&mut recv).await?);
    }

    expect(
        seen == ["stream-0", "stream-1", "stream-2"],
        format!("accepted {seen:?}"),
    )
}

/// The regression this harness exists for.
///
/// A browser `read()` cannot be cancelled: once issued, the browser hands the next
/// stream to *that* request. A clone that polls accept and is then dropped used to
/// take the next stream down with it, and the surviving handle waited forever.
///
/// The clone is polled by hand rather than awaited, so its read is definitely
/// outstanding before the peer opens anything.
async fn orphaned_accept(session: Session) -> Result<String, Error> {
    let doomed = session.clone();

    // One poll, with a waker that goes nowhere: enough to issue the browser read.
    let mut cx = Context::from_waker(Waker::noop());
    match doomed.poll_accept_uni(&mut cx) {
        Poll::Pending => {}
        Poll::Ready(Ok(_)) => {
            return expect(false, "a stream arrived before one was asked for".into())
        }
        Poll::Ready(Err(err)) => return Err(err),
    }

    // Now ask the peer for one, and drop the handle whose read is outstanding.
    let mut ack = command(&session, "streams 1").await?;
    read_all(&mut ack).await?;
    drop(doomed);

    let mut recv = timeout(session.accept_uni(), 3_000)
        .await
        .map_err(|_| stalled("accept_uni never resolved: the dropped clone ate the stream"))??;

    let got = read_all(&mut recv).await?;
    expect(got == "stream-0", format!("accepted {got:?}"))
}

/// The other regression.
///
/// `closed()` used to defer while bytes sat in our buffer, waiting for the caller to
/// drain them -- but it borrows `&mut self`, so a caller waiting there is a caller
/// who cannot read. Reading one byte at a time guarantees a leftover buffer.
async fn closed_with_buffered(session: Session) -> Result<String, Error> {
    let mut recv = command(&session, "echo abcdefgh").await?;

    // A single byte, leaving the rest of the browser's chunk buffered in the stream.
    let first = recv.read(1).await?;
    if first.as_deref() != Some(b"a".as_slice()) {
        return expect(false, format!("first read was {first:?}"));
    }

    timeout(recv.closed(), 3_000)
        .await
        .map_err(|_| stalled("closed() hung behind buffered bytes"))??;

    // And the bytes we had not taken yet are still there afterwards.
    let rest = read_all(&mut recv).await?;
    expect(rest == "bcdefgh", format!("rest was {rest:?}"))
}

/// A peer RESET_STREAM arrives with its code intact.
async fn peer_reset_code(session: Session) -> Result<String, Error> {
    let mut recv = command(&session, "reset 42").await?;

    let code = timeout(recv.closed(), 3_000)
        .await
        .map_err(|_| stalled("closed() never saw the reset"))?;

    match code {
        Ok(Some(42)) => expect(true, "code 42".to_string()),
        other => expect(false, format!("got {other:?}")),
    }
}

async fn datagram_round_trip(session: Session) -> Result<String, Error> {
    session.send_datagram(b"ping".as_slice().into()).await?;

    let got = timeout(session.recv_datagram(), 3_000)
        .await
        .map_err(|_| stalled("no datagram came back"))??;

    expect(
        got.as_ref() == b"ping",
        format!("received {:?}", String::from_utf8_lossy(&got)),
    )
}

/// A session close carries its code and reason out through the error.
async fn session_close(session: Session) -> Result<String, Error> {
    let mut ack = command(&session, "close 7 goodbye").await?;
    let _ = read_all(&mut ack).await;

    let err = timeout(session.closed(), 3_000)
        .await
        .map_err(|_| stalled("closed() never resolved"))?;

    use web_transport_trait::Error as _;
    match err.session_error() {
        Some((7, reason)) if reason.contains("goodbye") => {
            expect(true, format!("code 7, reason {reason:?}"))
        }
        other => expect(false, format!("session_error() was {other:?}")),
    }
}

// --- helpers ------------------------------------------------------------------

fn expect(ok: bool, detail: String) -> Result<String, Error> {
    if ok {
        Ok(detail)
    } else {
        Err(stalled(&detail))
    }
}

/// A stand-in error for a harness assertion, so a check can fail like any other.
fn stalled(detail: &str) -> Error {
    Error::Unknown(JsValue::from_str(detail))
}

fn row(name: &str, ok: bool, detail: &str) -> Object {
    let row = Object::new();
    let _ = Reflect::set(&row, &"name".into(), &name.into());
    let _ = Reflect::set(&row, &"ok".into(), &ok.into());
    let _ = Reflect::set(&row, &"detail".into(), &detail.into());
    row
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    let hex = hex.trim();
    if !hex.len().is_multiple_of(2) {
        return None;
    }

    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

/// Resolve `future`, or `Err(())` once `millis` have passed.
///
/// A hang is the failure mode for most of these checks, and a hung check would
/// otherwise take the whole page down with it.
async fn timeout<T>(future: impl std::future::Future<Output = T>, millis: i32) -> Result<T, ()> {
    let elapsed = Rc::new(std::cell::Cell::new(false));

    let sleep = {
        let elapsed = elapsed.clone();
        async move {
            let promise = js_sys::Promise::new(&mut |resolve, _| {
                let window = web_sys::window().expect("no window");
                let _ =
                    window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis);
            });
            let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
            elapsed.set(true);
        }
    };

    futures::pin_mut!(future, sleep);

    let output = futures::future::select(future, sleep).await;
    match output {
        futures::future::Either::Left((value, _)) => Ok(value),
        futures::future::Either::Right(_) => Err(()),
    }
}
