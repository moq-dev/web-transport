#![cfg(target_family = "wasm")]

//! A browser harness for the poll state machine.
//!
//! `just test-browser` runs this against Chromium and the real QUIC peer from
//! `web-transport-quinn`. The peer is driven by the commands sent here; use
//! `just harness` for interactive inspection.
//!
//! Each check returns a row rather than panicking, so one failure does not hide the
//! rest.

use std::{
    future::poll_fn,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Poll, Wake, Waker},
};

use js_sys::{Array, Object, Reflect};
use wasm_bindgen::prelude::*;
use web_transport_wasm::{ClientBuilder, Error, RecvStream, Session};

// The real Streams implementation supplies the writer state machine. Only the
// WebTransport getters are stand-ins: Chromium's session teardown errors its
// datagram writable, so it cannot demonstrate a clean writer closure.
#[wasm_bindgen(inline_js = "
export function closingDatagramTransport() {
    let finish;
    const stream = new WritableStream({
        close() { return new Promise(resolve => { finish = resolve; }); }
    }, { highWaterMark: 0 });
    const closed = stream.close();
    return {
        transport: { protocol: '', datagrams: { createWritable: () => stream } },
        finish: () => finish(),
        closed,
    };
}

export function openDatagramTransport() {
    let controller;
    const failure = new Error('controlled cleanup');
    const stream = new WritableStream({ start(value) { controller = value; } });
    return {
        transport: { protocol: '', datagrams: { createWritable: () => stream } },
        error: () => controller.error(failure),
        failure,
    };
}

export async function flushMicrotasks() {
    for (let i = 0; i < 8; i++) await Promise.resolve();
}
")]
extern "C" {
    #[wasm_bindgen(js_name = closingDatagramTransport)]
    fn closing_datagram_transport() -> Object;
    #[wasm_bindgen(js_name = openDatagramTransport)]
    fn open_datagram_transport() -> Object;
    #[wasm_bindgen(js_name = flushMicrotasks)]
    async fn flush_microtasks();
}

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

    results.push(&row(
        "malformed certificate hash is rejected",
        decode_hex("é").is_none(),
        "non-ASCII input returned None without panicking",
    ));

    for (name, already_closed) in [
        ("closing datagram writer terminates pending sends", false),
        ("cleanly closed datagram writer rejects new sends", true),
    ] {
        let outcome = clean_datagram_writer_terminates(already_closed).await;
        let outcome = match outcome {
            Ok(detail) => row(name, true, &detail),
            Err(err) => row(name, false, &format!("{err}")),
        };
        results.push(&outcome);
    }

    let outcome = dropped_datagram_senders_release_wakers().await;
    let outcome = match outcome {
        Ok(detail) => row(
            "dropped datagram senders release their wakers",
            true,
            &detail,
        ),
        Err(err) => row(
            "dropped datagram senders release their wakers",
            false,
            &format!("{err}"),
        ),
    };
    results.push(&outcome);

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
    check!("no subprotocol reports None", no_subprotocol_is_none);
    check!(
        "dropped pending bi opens release stream credit",
        dropped_pending_opens_release_credit
    );
    check!(
        "dropped pending uni opens release stream credit",
        dropped_pending_uni_opens_release_credit
    );
    check!("accept_uni delivers every stream", accept_all);
    check!("a dropped clone does not swallow a stream", orphaned_accept);
    check!(
        "closed() resolves with bytes still buffered",
        closed_with_buffered
    );
    check!("peer reset code reaches the receiver", peer_reset_code);
    check!("datagram round trip", datagram_round_trip);
    check!(
        "session teardown terminates datagram sends",
        closed_datagram_writer_terminates
    );
    check!(
        "cloned poll sends honor shared capacity",
        cloned_poll_sends_honor_capacity
    );
    check!(
        "synchronous datagrams continue after settlement",
        synchronous_datagrams_continue
    );
    check!(
        "rejected stream write makes the stream terminal",
        rejected_write_is_terminal
    );
    check!(
        "concurrent cloned senders all get capacity",
        concurrent_datagram_senders
    );
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
    send.write(format!("{command}\n").as_bytes()).await?;
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

/// `connect` above offers no subprotocols, so the browser negotiates none and
/// reports `""`. The trait contract says an unnegotiated protocol is `None`, and a
/// `Some("")` reaching a caller gets matched against their protocol table as if the
/// peer had chosen something.
async fn no_subprotocol_is_none(session: Session) -> Result<String, Error> {
    let protocol = session.protocol();

    expect(
        protocol.is_none(),
        format!("protocol() returned {protocol:?}"),
    )
}

/// Dropping a handle cannot cancel the browser promise returned by open. Churn
/// past the peer's stream limit, then prove those abandoned opens were reset and
/// returned their credit rather than blocking the next real stream forever.
async fn dropped_pending_opens_release_credit(session: Session) -> Result<String, Error> {
    for _ in 0..110 {
        let doomed = session.clone();
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());

        match doomed.poll_open_bi(&mut cx) {
            Poll::Pending => {}
            Poll::Ready(Ok((mut send, mut recv))) => {
                send.reset(0);
                recv.stop(0);
            }
            Poll::Ready(Err(err)) => return Err(err),
        }
    }

    sleep(250).await;
    let got = timeout(
        async {
            let mut recv = command(&session, "echo credit returned").await?;
            read_all(&mut recv).await
        },
        3_000,
    )
    .await
    .map_err(|_| stalled("an abandoned open retained the peer's stream credit"))??;

    expect(got == "credit returned", format!("echoed {got:?}"))
}

/// Exercise the separate unidirectional open path past the peer's stream limit.
async fn dropped_pending_uni_opens_release_credit(session: Session) -> Result<String, Error> {
    for _ in 0..110 {
        let doomed = session.clone();
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        match doomed.poll_open_uni(&mut cx) {
            Poll::Pending => {}
            Poll::Ready(Ok(mut send)) => send.reset(0),
            Poll::Ready(Err(err)) => return Err(err),
        }
    }

    sleep(250).await;
    let got = timeout(
        async {
            let mut send = session.open_uni().await?;
            send.write(b"uni credit returned").await?;
            send.finish()?;
            session.recv_datagram().await
        },
        3_000,
    )
    .await
    .map_err(|_| stalled("an abandoned uni open retained the peer's stream credit"))??;

    expect(got == b"uni credit returned"[..], format!("echoed {got:?}"))
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
    let survivor = session.clone();
    let doomed_wake = Arc::new(FlagWake::default());
    let survivor_wake = Arc::new(FlagWake::default());

    // One poll with its own task identity: enough to issue the browser read.
    let doomed_waker = Waker::from(doomed_wake);
    let mut cx = Context::from_waker(&doomed_waker);
    match doomed.poll_accept_uni(&mut cx) {
        Poll::Pending => {}
        Poll::Ready(Ok(_)) => {
            return expect(false, "a stream arrived before one was asked for".into())
        }
        Poll::Ready(Err(err)) => return Err(err),
    }

    // Issue a newer read on the handle that survives. The older orphan must take
    // precedence over this request, even though this handle is no longer idle.
    let survivor_waker = Waker::from(survivor_wake.clone());
    let mut cx = Context::from_waker(&survivor_waker);
    match survivor.poll_accept_uni(&mut cx) {
        Poll::Pending => {}
        Poll::Ready(Ok(_)) => {
            return expect(false, "a stream arrived before one was asked for".into())
        }
        Poll::Ready(Err(err)) => return Err(err),
    }

    // The peer acknowledges before waiting to open the stream, making the drop
    // happen while both reads are still pending. The dropped task's waker cannot
    // help once the browser eventually fulfills its older request.
    let mut ack = command(&session, "streams-after 250 1").await?;
    read_all(&mut ack).await?;
    drop(doomed);

    if !survivor_wake.0.swap(false, Ordering::SeqCst) {
        return expect(
            false,
            "dropping the older read did not wake the survivor".into(),
        );
    }

    let mut recv = timeout(survivor.accept_uni(), 3_000).await.map_err(|_| {
        stalled("accept_uni never resolved: the surviving clone ignored an older orphan")
    })??;

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

/// Zero high-water mark keeps desiredSize at zero while close is controlled by
/// the sink. Streams resolves ready on close even though no capacity returns.
/// Bound the polls and advance only microtasks: a timer cannot interrupt the
/// original promise wake loop, which would starve the rest of the harness.
async fn clean_datagram_writer_terminates(already_closed: bool) -> Result<String, Error> {
    let fixture = closing_datagram_transport();
    let transport = Reflect::get(&fixture, &"transport".into())?.unchecked_into();
    let session = Session::new(transport, "https://controlled.invalid".parse().unwrap());
    let wake = Arc::new(FlagWake::default());
    let waker = Waker::from(wake.clone());
    let mut cx = Context::from_waker(&waker);

    if already_closed {
        finish_controlled_close(&fixture).await?;
    } else {
        if !session.poll_send_datagram(&mut cx, b"pending").is_pending() {
            return expect(false, "closing writer did not start pending".into());
        }
        flush_microtasks().await;
        if !wake.0.swap(false, Ordering::SeqCst) {
            return expect(
                false,
                "terminal readiness did not wake the pending send".into(),
            );
        }
    }

    let mut outcome = expect(
        false,
        "writer remained in a fulfilled-ready wake loop".into(),
    );
    for _ in 0..4 {
        match session.poll_send_datagram(&mut cx, b"late") {
            Poll::Ready(Err(Error::Closed)) => {
                outcome = expect(true, "terminal readiness returned Closed".into());
                break;
            }
            Poll::Ready(other) => {
                outcome = expect(false, format!("unexpected result: {other:?}"));
                break;
            }
            Poll::Pending => flush_microtasks().await,
        }
    }

    // The sink cannot finish until polling has terminated. A ready wake loop
    // would starve the browser task responsible for that completion.
    if !already_closed {
        finish_controlled_close(&fixture).await?;
    }
    outcome
}

async fn finish_controlled_close(fixture: &Object) -> Result<(), Error> {
    flush_microtasks().await;
    let finish: js_sys::Function = Reflect::get(fixture, &"finish".into())?.unchecked_into();
    finish.call0(fixture)?;
    let closed: js_sys::Promise = Reflect::get(fixture, &"closed".into())?.unchecked_into();
    wasm_bindgen_futures::JsFuture::from(closed).await?;
    Ok(())
}

/// Dropping a sender must release its task waker even while the writer stays open.
async fn dropped_datagram_senders_release_wakers() -> Result<String, Error> {
    let fixture = open_datagram_transport();
    let transport = Reflect::get(&fixture, &"transport".into())?.unchecked_into();
    let session = Session::new(transport, "https://controlled.invalid".parse().unwrap());
    let mut wakes = Vec::new();

    for _ in 0..32 {
        let wake = Arc::new(FlagWake::default());
        {
            let sender = session.clone();
            let waker = Waker::from(wake.clone());
            let mut cx = Context::from_waker(&waker);
            match sender.poll_send_datagram(&mut cx, b"once") {
                Poll::Ready(Ok(())) => {}
                other => return expect(false, format!("open writer send returned {other:?}")),
            }
        }
        flush_microtasks().await;
        wakes.push(wake);
    }

    let retained = wakes
        .iter()
        .filter(|wake| Arc::strong_count(wake) != 1)
        .count();
    let error: js_sys::Function = Reflect::get(&fixture, &"error".into())?.unchecked_into();
    error.call0(&fixture)?;
    flush_microtasks().await;
    let stale_wakes = wakes
        .iter()
        .filter(|wake| wake.0.load(Ordering::SeqCst))
        .count();

    let failure = Reflect::get(&fixture, &"failure".into())?;
    let mut original_error = false;
    let mut cx = Context::from_waker(futures::task::noop_waker_ref());
    for _ in 0..4 {
        match session.poll_send_datagram(&mut cx, b"late") {
            Poll::Ready(Err(Error::Unknown(err))) => {
                original_error = Object::is(&err, &failure);
                break;
            }
            Poll::Ready(_) => break,
            Poll::Pending => flush_microtasks().await,
        }
    }

    expect(
        retained == 0 && stale_wakes == 0 && original_error,
        format!("{retained} dropped send tasks retained, {stale_wakes} stale wakes, original error preserved: {original_error}"),
    )
}

async fn closed_datagram_writer_terminates(session: Session) -> Result<String, Error> {
    session.send_datagram(b"prime".as_slice().into()).await?;
    let _ = session.recv_datagram().await?;
    session.close(0, "done");
    let _ = timeout(session.closed(), 1_000)
        .await
        .map_err(|_| stalled("session did not close"))?;

    match timeout(poll_fn(|cx| session.poll_send_datagram(cx, b"late")), 1_000).await {
        Ok(Err(_)) => expect(true, "closed writer returned an error".into()),
        Ok(Ok(())) => expect(false, "closed writer accepted a datagram".into()),
        Err(()) => expect(false, "closed writer stayed in a wake loop".into()),
    }
}

/// Every clone shares one browser writer, so an idle per-clone operation slot must
/// not bypass the writer's shared backpressure signal.
async fn cloned_poll_sends_honor_capacity(session: Session) -> Result<String, Error> {
    let first = session.clone();
    let second = session.clone();
    let mut cx = Context::from_waker(futures::task::noop_waker_ref());

    match first.poll_send_datagram(&mut cx, b"first") {
        Poll::Ready(Ok(())) => {}
        Poll::Ready(Err(err)) => return Err(err),
        Poll::Pending => return expect(false, "the first datagram had no capacity".into()),
    }

    match second.poll_send_datagram(&mut cx, b"second") {
        Poll::Pending => expect(true, "the second clone observed shared backpressure".into()),
        Poll::Ready(Ok(())) => expect(false, "the second clone bypassed shared capacity".into()),
        Poll::Ready(Err(err)) => Err(err),
    }
}

/// A settled promise must not permanently occupy the synchronous trait adapter's
/// one retained slot. Receiving the first echo proves that write has completed
/// before a second datagram is offered.
async fn synchronous_datagrams_continue(session: Session) -> Result<String, Error> {
    web_transport_trait::Session::send_datagram(&session, b"first".as_slice().into())?;

    let first = timeout(session.recv_datagram(), 3_000)
        .await
        .map_err(|_| stalled("the first synchronous datagram was not echoed"))??;

    web_transport_trait::Session::send_datagram(&session, b"second".as_slice().into())?;

    let second = timeout(session.recv_datagram(), 3_000)
        .await
        .map_err(|_| stalled("a settled write blocked the second synchronous datagram"))??;

    expect(
        first.as_ref() == b"first" && second.as_ref() == b"second",
        format!(
            "received {:?}, then {:?}",
            String::from_utf8_lossy(&first),
            String::from_utf8_lossy(&second)
        ),
    )
}

async fn rejected_write_is_terminal(session: Session) -> Result<String, Error> {
    let (mut send, _recv) = session.open_bi().await?;
    send.write(b"stop 42\n").await?;

    let _first_error = timeout(
        async {
            loop {
                match send.write(b"trigger").await {
                    Ok(()) => sleep(10).await,
                    Err(err) => break err,
                }
            }
        },
        1_000,
    )
    .await
    .map_err(|_| stalled("peer STOP_SENDING never rejected a write"))?;

    let mut cx = Context::from_waker(futures::task::noop_waker_ref());
    match send.poll_write(&mut cx, b"retry") {
        Poll::Ready(Err(Error::Closed)) => expect(true, "retry returned Closed".into()),
        Poll::Ready(Err(err)) => expect(false, format!("retry returned {err}")),
        Poll::Ready(Ok(size)) => expect(false, format!("retry accepted {size} bytes")),
        Poll::Pending => expect(false, "retry remained pending after failure".into()),
    }
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

/// Cloned senders share one browser writer, so they share its backpressure.
///
/// When two are parked on the same `ready` promise and capacity returns, only one
/// wins. The loser sees the capacity gone again -- and the `ready` it holds has
/// already fulfilled and never will again, because the writer swaps in a fresh
/// promise. It has to resubscribe to that new one. Falling back to `closed()`
/// instead parks it for the life of the session, which is a deadlock dressed up as
/// backpressure.
///
/// Driven by hand rather than by volume: the race needs two clones parked on one
/// promise at the same instant, and a loop of `await`s lets capacity recover
/// between polls often enough to miss it entirely.
async fn concurrent_datagram_senders(session: Session) -> Result<String, Error> {
    let winner = session.clone();
    let loser = session.clone();
    let mut cx = Context::from_waker(futures::task::noop_waker_ref());

    // Take the writer's only slot, so the other two have to wait.
    match session.poll_send_datagram(&mut cx, b"first") {
        Poll::Ready(Ok(())) => {}
        Poll::Ready(Err(err)) => return Err(err),
        Poll::Pending => return expect(false, "the first datagram had no capacity".into()),
    }

    // Both park on the *same* ready promise, which is what sets up the race.
    for (label, clone) in [("winner", &winner), ("loser", &loser)] {
        match clone.poll_send_datagram(&mut cx, b"queued") {
            Poll::Pending => {}
            Poll::Ready(Ok(())) => {
                return expect(false, format!("the {label} bypassed shared capacity"))
            }
            Poll::Ready(Err(err)) => return Err(err),
        }
    }

    // Let capacity come back, then let one of them take it.
    let mut won = false;
    for _ in 0..20 {
        sleep(25).await;
        match winner.poll_send_datagram(&mut cx, b"winner") {
            Poll::Ready(Ok(())) => {
                won = true;
                break;
            }
            Poll::Ready(Err(err)) => return Err(err),
            Poll::Pending => continue,
        }
    }

    if !won {
        return expect(false, "capacity never returned for the winner".into());
    }

    // The loser is now holding a fulfilled `ready` with no capacity behind it.
    timeout(poll_fn(|cx| loser.poll_send_datagram(cx, b"loser")), 5_000)
        .await
        .map_err(|_| stalled("the clone that lost the capacity race never woke again"))??;

    expect(true, "the loser resubscribed and sent".into())
}

// --- helpers ------------------------------------------------------------------

#[derive(Default)]
struct FlagWake(AtomicBool);

impl Wake for FlagWake {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

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
    let hex = hex.trim().as_bytes();
    let pairs = hex.chunks_exact(2);
    if !pairs.remainder().is_empty() {
        return None;
    }

    pairs
        .map(|pair| {
            let pair = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(pair, 16).ok()
        })
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

async fn sleep(millis: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let window = web_sys::window().expect("no window");
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
