use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    future::poll_fn,
    rc::Rc,
    task::{ready, Context, Poll, Waker},
};

use bytes::Bytes;
use js_sys::{Function, Reflect, Uint8Array};
use url::Url;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    ReadableStream, ReadableStreamDefaultReader, WebTransport, WebTransportBidirectionalStream,
    WebTransportCloseInfo, WebTransportDatagramDuplexStream, WebTransportSendStream,
    WebTransportSendStreamOptions, WritableStream, WritableStreamDefaultWriter,
};

use crate::{
    error::session_error,
    js::{promise, read_value, Op},
    Error, RecvStream, SendStream,
};

/// A session represents a connection between a client and a server.
///
/// This is the main entry point for creating new streams and sending datagrams.
/// The session can be closed by either endpoint with an error code and reason.
///
/// The session can be cloned to run operations concurrently: each clone keeps its
/// own in-flight operation, which is what the poll interface asks of a caller that
/// wants two at once.
#[derive(Clone)]
pub struct Session {
    shared: Rc<Shared>,

    accept_uni: IncomingOp,
    accept_bi: IncomingOp,
    open_uni: Op,
    open_bi: Op,
    send_datagram: Op,
    recv_datagram: IncomingOp,
    closed: Op,
}

/// State that every clone of a session shares.
///
/// The stream locks live here because a browser stream can only be locked once;
/// duplicating them would throw. Sharing them costs a clone nothing, because it
/// still calls `read()` for itself — the streams spec queues concurrent reads and
/// fulfills them in order, so one clone's pending read is never handed to another.
struct Shared {
    inner: WebTransport,
    url: Url,
    protocol: Option<String>,

    accept_uni: Incoming,
    accept_bi: Incoming,
    datagrams: Incoming,
    send_datagram: RefCell<Option<WritableStreamDefaultWriter>>,
}

/// One browser stream this session reads from.
///
/// The reader is shared because a browser stream can only be locked once. So are
/// the orphans, for a subtler reason: a `read()` cannot be cancelled. Once it is
/// issued the browser will hand the next value to *that* request, so a read whose
/// handle was dropped has to be picked up by a surviving clone or the stream it was
/// about to deliver is stranded and nobody ever sees it.
#[derive(Default)]
struct Incoming {
    reader: RefCell<Option<ReadableStreamDefaultReader>>,
    next_order: Cell<u64>,
    orphans: RefCell<BTreeMap<u64, JsFuture>>,
    waiters: RefCell<BTreeMap<u64, Waker>>,
}

impl Incoming {
    /// Lock `stream` on first use, handing out the reader every time after.
    ///
    /// `stream` is a closure so the browser getter behind it runs once rather than
    /// on every poll.
    fn reader(
        &self,
        stream: impl FnOnce() -> ReadableStream,
    ) -> Result<ReadableStreamDefaultReader, Error> {
        let mut slot = self.reader.borrow_mut();

        match slot.as_ref() {
            Some(reader) => Ok(reader.clone()),
            None => {
                let reader = ReadableStreamDefaultReader::new(&stream())?;
                *slot = Some(reader.clone());
                Ok(reader)
            }
        }
    }
}

/// One handle's read from an [`Incoming`] browser stream.
///
/// The order follows the browser's read-request order. It lets a surviving handle
/// adopt an older orphan without moving its own older request behind a newer one.
#[derive(Default)]
struct IncomingOp {
    order: Cell<Option<u64>>,
    future: Op,
}

impl IncomingOp {
    /// Replace this handle's read with an older orphan, returning the displaced read.
    fn replace(&self, order: u64, future: JsFuture) -> Option<(u64, JsFuture)> {
        let previous_order = self.order.replace(Some(order));
        let previous_future = self.future.replace(future);

        match (previous_order, previous_future) {
            (Some(order), Some(future)) => Some((order, future)),
            (None, None) => None,
            _ => unreachable!("incoming read order and future must move together"),
        }
    }

    /// Take this handle's read so another clone can adopt it.
    fn take(&self) -> Option<(u64, JsFuture)> {
        let future = self.future.take()?;
        let order = self
            .order
            .take()
            .expect("an incoming read future always has an order");
        Some((order, future))
    }

    /// Poll the assigned read, starting a fresh one with `order` when idle.
    fn poll_with(
        &self,
        cx: &mut Context<'_>,
        order: u64,
        make: impl FnOnce() -> JsFuture,
    ) -> Poll<Result<JsValue, Error>> {
        let result = self.future.poll_with(cx, || {
            assert!(
                self.order.replace(Some(order)).is_none(),
                "an idle incoming read cannot already have an order"
            );
            make()
        });

        if result.is_ready() {
            self.order.set(None);
        }

        result
    }
}

/// A cloned session handle starts with no read assigned to it.
impl Clone for IncomingOp {
    fn clone(&self) -> Self {
        Self::default()
    }
}

/// Whether the oldest orphan must run before this handle's assigned read.
fn should_adopt_orphan(current: Option<u64>, orphan: u64) -> bool {
    match current {
        Some(current) => orphan < current,
        None => true,
    }
}

/// Whether the browser writer currently has room for another datagram.
fn datagram_has_capacity(desired_size: Option<f64>) -> bool {
    matches!(desired_size, Some(size) if size > 0.0)
}

/// The datagram writer. The current spec exposes it via `createWritable()`; the
/// `.writable` property is deprecated, non-standard, and unimplemented by Safari
/// (there `.writable` is `undefined`, so datagram sending throws). Prefer
/// `createWritable()`, falling back to `.writable` for browsers that still have it.
/// MDN — the `.writable` deprecation note and the feature-detect example:
/// <https://developer.mozilla.org/en-US/docs/Web/API/WebTransportDatagramDuplexStream/writable>
/// <https://developer.mozilla.org/en-US/docs/Web/API/WebTransport/datagrams#writing_an_outgoing_datagram>
fn datagram_writable(dg: &WebTransportDatagramDuplexStream) -> WritableStream {
    Reflect::get(dg, &"createWritable".into())
        .ok()
        .and_then(|f| f.dyn_into::<Function>().ok())
        .and_then(|f| f.call0(dg).ok())
        .and_then(|ws| ws.dyn_into::<WritableStream>().ok())
        .unwrap_or_else(|| dg.writable())
}

/// Split a browser bidirectional stream into our two halves.
fn bi_streams(stream: WebTransportBidirectionalStream) -> Result<(SendStream, RecvStream), Error> {
    let send = SendStream::new(stream.writable())?;
    let recv = RecvStream::new(stream.readable())?;
    Ok((send, recv))
}

/// Options used when opening a stream.
///
/// `waitUntilAvailable` asks the browser to wait for stream credit when the peer's
/// concurrent stream limit is exhausted, instead of failing the returned promise.
/// Waiting is what the other backends do, and what `open_uni`/`open_bi` promise.
/// <https://www.w3.org/TR/webtransport/#dom-webtransportsendstreamoptions-waituntilavailable>
///
/// No engine honors it yet: Chromium omits the member pending
/// <https://crbug.com/487117768>, WebKit has it commented out, and Gecko's
/// dictionary carries only `sendGroup`/`sendOrder`. Unknown dictionary members
/// are ignored, so this is inert until they ship it rather than an error.
///
/// TODO use the web_sys setter when the binding exists; the dictionary currently
/// only exposes `sendOrder`.
fn stream_options() -> WebTransportSendStreamOptions {
    let options = WebTransportSendStreamOptions::new();
    Reflect::set(&options, &"waitUntilAvailable".into(), &JsValue::TRUE)
        .expect("failed to set waitUntilAvailable");
    options
}

impl Session {
    pub fn new(inner: WebTransport, url: Url) -> Self {
        // TODO use the web_sys bindings when updated.
        // Until then, we try to access the protocol property on the inner object.
        let protocol = Reflect::get(&inner, &"protocol".into())
            .ok()
            .and_then(|p| p.as_string());

        Self {
            shared: Rc::new(Shared {
                inner,
                url,
                protocol,
                accept_uni: Incoming::default(),
                accept_bi: Incoming::default(),
                datagrams: Incoming::default(),
                send_datagram: RefCell::new(None),
            }),
            accept_uni: IncomingOp::default(),
            accept_bi: IncomingOp::default(),
            open_uni: Op::default(),
            open_bi: Op::default(),
            send_datagram: Op::default(),
            recv_datagram: IncomingOp::default(),
            closed: Op::default(),
        }
    }

    /// Poll for a unidirectional stream created by the peer.
    pub fn poll_accept_uni(&self, cx: &mut Context<'_>) -> Poll<Result<RecvStream, Error>> {
        let inner = &self.shared.inner;
        let result = ready!(self.poll_incoming(
            cx,
            &self.accept_uni,
            &self.shared.accept_uni,
            || { inner.incoming_unidirectional_streams() }
        ))?;

        match read_value(result) {
            Some(stream) => Poll::Ready(RecvStream::new(stream)),
            // The queue only ends when the session does, so ask why.
            None => Poll::Ready(Err(ready!(self.poll_closed(cx)))),
        }
    }

    /// Poll for a bidirectional stream created by the peer.
    pub fn poll_accept_bi(
        &self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(SendStream, RecvStream), Error>> {
        let inner = &self.shared.inner;
        let result = ready!(self.poll_incoming(
            cx,
            &self.accept_bi,
            &self.shared.accept_bi,
            || { inner.incoming_bidirectional_streams() }
        ))?;

        match read_value(result) {
            Some(stream) => Poll::Ready(bi_streams(stream)),
            None => Poll::Ready(Err(ready!(self.poll_closed(cx)))),
        }
    }

    /// Poll to open a unidirectional stream, which blocks while there are too many
    /// concurrent streams.
    pub fn poll_open_uni(&self, cx: &mut Context<'_>) -> Poll<Result<SendStream, Error>> {
        let inner = &self.shared.inner;
        let stream = ready!(self.open_uni.poll(cx, || {
            promise(inner.create_unidirectional_stream_with_options(&stream_options()))
        }))?;

        Poll::Ready(SendStream::new(stream.unchecked_into()))
    }

    /// Poll to open a bidirectional stream, which blocks while there are too many
    /// concurrent streams.
    pub fn poll_open_bi(
        &self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(SendStream, RecvStream), Error>> {
        let inner = &self.shared.inner;
        let stream = ready!(self.open_bi.poll(cx, || {
            promise(inner.create_bidirectional_stream_with_options(&stream_options()))
        }))?;

        Poll::Ready(bi_streams(stream.unchecked_into()))
    }

    /// Poll to send a datagram over the network.
    ///
    /// Returns [`Poll::Pending`] until the browser has accepted the *previous*
    /// datagram, so a caller waits for capacity instead of piling up payloads. The
    /// payload is untouched while pending.
    pub fn poll_send_datagram(
        &self,
        cx: &mut Context<'_>,
        payload: &[u8],
    ) -> Poll<Result<(), Error>> {
        // Draining the previous write is what bounds this path: one datagram
        // outstanding per handle, never a queue.
        ready!(self.send_datagram.poll_settled(cx))?;

        let writer = match self.datagram_writer() {
            Ok(writer) => writer,
            Err(err) => return Poll::Ready(Err(err)),
        };

        // The writer is shared by every clone, so a local idle slot does not mean
        // the browser has room. Wait on the shared backpressure promise and
        // recheck: another clone may consume capacity before this one is polled.
        loop {
            match writer.desired_size() {
                Ok(size) if datagram_has_capacity(size) => break,
                Ok(_) => match self.send_datagram.poll(cx, || promise(writer.ready())) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(_)) => match writer.desired_size() {
                        Ok(size) if datagram_has_capacity(size) => continue,
                        // `ready` also fulfills when the writer closes. Its closed
                        // promise distinguishes that terminal zero from ordinary
                        // backpressure and prevents an already-ready wake loop.
                        Ok(_) => match self.send_datagram.poll(cx, || promise(writer.closed())) {
                            Poll::Pending => return Poll::Pending,
                            Poll::Ready(Ok(_)) => return Poll::Ready(Err(Error::Closed)),
                            Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                        },
                        Err(err) => return Poll::Ready(Err(err.into())),
                    },
                    Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                },
                Err(err) => return Poll::Ready(Err(err.into())),
            }
        }

        self.write_datagram(&writer, payload);
        Poll::Ready(Ok(()))
    }

    /// Hand a datagram to the browser if it has room, reporting whether it took it.
    ///
    /// Unlike [`poll_send_datagram`](Self::poll_send_datagram) this never waits, so
    /// it has to drop rather than queue: a datagram offered while the browser
    /// reports no room is discarded.
    /// Queueing instead would grow without bound on a stalled connection, and
    /// nothing downstream discards the excess — a browser write cannot be
    /// cancelled, so the backlog would eventually go out stale. Dropping is a
    /// datagram's normal fate anyway; congestion, loss and size do it too.
    pub fn try_send_datagram(&self, payload: &[u8]) -> Result<bool, Error> {
        let writer = self.datagram_writer()?;

        // `desiredSize` is the writer's own backpressure signal: at or below zero
        // the queue is already at its high water mark. `None` is an errored stream,
        // which has no capacity either. A prior write future may still occupy our
        // slot after it settled because this synchronous path cannot poll it; live
        // writer capacity, rather than that stale slot, decides whether to send.
        if !datagram_has_capacity(writer.desired_size()?) {
            return Ok(false);
        }

        self.write_datagram(&writer, payload);
        Ok(true)
    }

    /// Lock the datagram writable on first use, handing out the writer after.
    fn datagram_writer(&self) -> Result<WritableStreamDefaultWriter, Error> {
        let mut slot = self.shared.send_datagram.borrow_mut();

        match slot.as_ref() {
            Some(writer) => Ok(writer.clone()),
            None => {
                let writable = datagram_writable(&self.shared.inner.datagrams());
                let writer = WritableStreamDefaultWriter::new(&writable)?;
                *slot = Some(writer.clone());
                Ok(writer)
            }
        }
    }

    /// Hand a datagram over, keeping the write as this handle's in-flight operation.
    fn write_datagram(&self, writer: &WritableStreamDefaultWriter, payload: &[u8]) {
        self.send_datagram
            .start(promise(writer.write_with_chunk(&Uint8Array::from(payload))));
    }

    /// Poll for a datagram from the network.
    pub fn poll_recv_datagram(&self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Error>> {
        let inner = &self.shared.inner;
        let result = ready!(self.poll_incoming(
            cx,
            &self.recv_datagram,
            &self.shared.datagrams,
            || { inner.datagrams().readable() }
        ))?;

        match read_value::<Uint8Array>(result) {
            Some(data) => Poll::Ready(Ok(data.to_vec().into())),
            None => Poll::Ready(Err(ready!(self.poll_closed(cx)))),
        }
    }

    /// Poll until the session is closed by either side.
    pub fn poll_closed(&self, cx: &mut Context<'_>) -> Poll<Error> {
        let inner = &self.shared.inner;

        // A rejection is a session that failed rather than closed; it already
        // describes itself.
        let info = match ready!(self.closed.poll(cx, || promise(inner.closed()))) {
            Ok(info) => info,
            Err(err) => return Poll::Ready(err),
        };

        let info: WebTransportCloseInfo = info.unchecked_into();
        Poll::Ready(session_error(&info))
    }

    /// Poll a read against one of the shared browser queues.
    ///
    /// Adopts a read orphaned by a dropped handle before issuing a new one, which
    /// is what keeps that handle's disappearance from swallowing a stream. The
    /// browser fulfills read requests in the order they were made, so taking the
    /// oldest orphan first also preserves that order.
    fn poll_incoming(
        &self,
        cx: &mut Context<'_>,
        op: &IncomingOp,
        incoming: &Incoming,
        stream: impl FnOnce() -> ReadableStream,
    ) -> Poll<Result<JsValue, Error>> {
        let reader = match incoming.reader(stream) {
            Ok(reader) => reader,
            Err(err) => return Poll::Ready(Err(err)),
        };

        let oldest_orphan = incoming
            .orphans
            .borrow()
            .first_key_value()
            .map(|(&order, _)| order);

        if let Some(order) =
            oldest_orphan.filter(|&order| should_adopt_orphan(op.order.get(), order))
        {
            let future = incoming
                .orphans
                .borrow_mut()
                .remove(&order)
                .expect("the oldest orphan cannot disappear on one thread");

            if let Some((displaced_order, displaced_future)) = op.replace(order, future) {
                incoming.waiters.borrow_mut().remove(&displaced_order);
                let previous = incoming
                    .orphans
                    .borrow_mut()
                    .insert(displaced_order, displaced_future);
                assert!(previous.is_none(), "incoming read orders are unique");
            }
        }

        let order = op.order.get().unwrap_or_else(|| {
            let order = incoming.next_order.get();
            incoming
                .next_order
                .set(order.checked_add(1).expect("incoming read order exhausted"));
            order
        });

        incoming
            .waiters
            .borrow_mut()
            .insert(order, cx.waker().clone());

        let result = op.poll_with(cx, order, || JsFuture::from(promise(reader.read())));
        if result.is_ready() {
            incoming.waiters.borrow_mut().remove(&order);
        }
        result
    }

    /// Accept a new unidirectional stream from the peer.
    pub async fn accept_uni(&self) -> Result<RecvStream, Error> {
        poll_fn(|cx| self.poll_accept_uni(cx)).await
    }

    /// Accept a new bidirectional stream from the peer.
    pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), Error> {
        poll_fn(|cx| self.poll_accept_bi(cx)).await
    }

    /// Creates a new bidirectional stream.
    ///
    /// This asks the browser to wait, rather than fail, when the peer's concurrent
    /// stream limit has been reached. No engine implements that yet, so this can
    /// still return an error on exhaustion.
    pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), Error> {
        poll_fn(|cx| self.poll_open_bi(cx)).await
    }

    /// Creates a new unidirectional stream.
    ///
    /// This asks the browser to wait, rather than fail, when the peer's concurrent
    /// stream limit has been reached. No engine implements that yet, so this can
    /// still return an error on exhaustion.
    pub async fn open_uni(&self) -> Result<SendStream, Error> {
        poll_fn(|cx| self.poll_open_uni(cx)).await
    }

    /// Send a datagram over the network.
    pub async fn send_datagram(&self, payload: Bytes) -> Result<(), Error> {
        poll_fn(|cx| self.poll_send_datagram(cx, &payload)).await
    }

    /// Receive a datagram over the network.
    pub async fn recv_datagram(&self) -> Result<Bytes, Error> {
        poll_fn(|cx| self.poll_recv_datagram(cx)).await
    }

    /// The maximum size of a datagram that can be sent.
    pub fn max_datagram_size(&self) -> usize {
        self.shared.inner.datagrams().max_datagram_size() as usize
    }

    /// Close the session with the given error code and reason.
    pub fn close(&self, code: u32, reason: &str) {
        let info = WebTransportCloseInfo::new();
        info.set_close_code(code);
        info.set_reason(reason);
        self.shared.inner.close_with_close_info(&info);
    }

    /// Block until the session is closed and return the error.
    pub async fn closed(&self) -> Error {
        poll_fn(|cx| self.poll_closed(cx)).await
    }

    /// Return the URL used to create the session.
    pub fn url(&self) -> &Url {
        &self.shared.url
    }

    /// Return the application protocol used to create the session.
    pub fn protocol(&self) -> Option<&str> {
        self.shared.protocol.as_deref()
    }
}

impl Drop for Session {
    /// Preserve or dispose of browser operations that outlive this handle.
    ///
    /// A browser read request cannot be cancelled, so the browser will still hand
    /// the next stream or datagram to it. Dropping it here would strand that value:
    /// the request is satisfied, nothing is listening, and no surviving clone ever
    /// learns the stream existed.
    ///
    fn drop(&mut self) {
        for (op, incoming) in [
            (&self.accept_uni, &self.shared.accept_uni),
            (&self.accept_bi, &self.shared.accept_bi),
            (&self.recv_datagram, &self.shared.datagrams),
        ] {
            if let Some((order, future)) = op.take() {
                incoming.waiters.borrow_mut().remove(&order);
                let previous = incoming.orphans.borrow_mut().insert(order, future);
                assert!(previous.is_none(), "incoming read orders are unique");

                // A clone may already be parked on a newer browser read. Wake it
                // now so it can adopt this older future and register its waker on
                // that request before the browser fulfills it.
                for waiter in incoming.waiters.borrow().values() {
                    waiter.wake_by_ref();
                }
            }
        }

        // Browser open promises cannot be cancelled either. If one settles after
        // its handle disappears, close the resulting stream so it does not retain
        // peer stream credit forever.
        if let Some(future) = self.open_uni.take() {
            wasm_bindgen_futures::spawn_local(async move {
                if let Ok(stream) = future.await {
                    let stream: WebTransportSendStream = stream.unchecked_into();
                    if let Ok(mut stream) = SendStream::new(stream) {
                        stream.reset(0);
                    }
                }
            });
        }

        if let Some(future) = self.open_bi.take() {
            wasm_bindgen_futures::spawn_local(async move {
                if let Ok(stream) = future.await {
                    let stream: WebTransportBidirectionalStream = stream.unchecked_into();
                    if let Ok((mut send, mut recv)) = bi_streams(stream) {
                        send.reset(0);
                        recv.stop(0);
                    }
                }
            });
        }
    }
}

impl PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        self.shared.inner == other.shared.inner
    }
}

impl Eq for Session {}

#[cfg(test)]
mod tests {
    use super::{datagram_has_capacity, should_adopt_orphan};

    #[test]
    fn only_older_orphans_preempt_an_inflight_read() {
        assert!(should_adopt_orphan(Some(2), 1));
        assert!(!should_adopt_orphan(Some(1), 2));
        assert!(should_adopt_orphan(None, 2));
    }

    #[test]
    fn settled_datagram_slot_does_not_override_live_writer_capacity() {
        assert!(datagram_has_capacity(Some(1.0)));
        assert!(!datagram_has_capacity(Some(0.0)));
        assert!(!datagram_has_capacity(Some(-1.0)));
        assert!(!datagram_has_capacity(None));
    }
}
