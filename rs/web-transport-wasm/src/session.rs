use std::{
    cell::RefCell,
    future::poll_fn,
    rc::Rc,
    task::{ready, Context, Poll},
};

use bytes::Bytes;
use js_sys::{Function, Reflect, Uint8Array};
use url::Url;
use wasm_bindgen::JsCast;
use web_sys::{
    ReadableStream, ReadableStreamDefaultReader, WebTransport, WebTransportBidirectionalStream,
    WebTransportCloseInfo, WebTransportDatagramDuplexStream, WritableStream,
    WritableStreamDefaultWriter,
};

use crate::{
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

    accept_uni: Op,
    accept_bi: Op,
    open_uni: Op,
    open_bi: Op,
    send_datagram: Op,
    recv_datagram: Op,
    closed: Op,
}

/// State that every clone of a session shares.
///
/// The readers and the datagram writer are here rather than on the handle because a
/// browser stream can only be locked once; duplicating them would throw. Sharing
/// them costs nothing, because each clone still calls `read()` for itself — the
/// streams spec queues concurrent reads and fulfills them in order, so one clone's
/// pending read is never handed to another.
struct Shared {
    inner: WebTransport,
    url: Url,
    protocol: Option<String>,

    accept_uni: RefCell<Option<ReadableStreamDefaultReader>>,
    accept_bi: RefCell<Option<ReadableStreamDefaultReader>>,
    recv_datagram: RefCell<Option<ReadableStreamDefaultReader>>,
    send_datagram: RefCell<Option<WritableStreamDefaultWriter>>,
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

/// Lock `stream` on first use, handing out the reader every time after.
///
/// `stream` is a closure so the browser getter behind it runs once rather than on
/// every poll.
fn reader(
    slot: &RefCell<Option<ReadableStreamDefaultReader>>,
    stream: impl FnOnce() -> ReadableStream,
) -> Result<ReadableStreamDefaultReader, Error> {
    let mut slot = slot.borrow_mut();

    match slot.as_ref() {
        Some(reader) => Ok(reader.clone()),
        None => {
            let reader = ReadableStreamDefaultReader::new(&stream())?;
            *slot = Some(reader.clone());
            Ok(reader)
        }
    }
}

/// Split a browser bidirectional stream into our two halves.
fn bi_streams(stream: WebTransportBidirectionalStream) -> Result<(SendStream, RecvStream), Error> {
    let send = SendStream::new(stream.writable())?;
    let recv = RecvStream::new(stream.readable())?;
    Ok((send, recv))
}

/// The error described by a close info, which is how a clean close reaches a caller.
fn session_error(info: WebTransportCloseInfo) -> Error {
    let reason = info.get_reason().unwrap_or_default();

    let options = web_sys::WebTransportErrorOptions::new();
    options.set_source(web_sys::WebTransportErrorSource::Session);

    // `WebTransportError` carries the code as a byte, so an application close code
    // above 255 arrives as none rather than as some other code's meaning.
    options.set_stream_error_code(
        info.get_close_code()
            .and_then(|code| u8::try_from(code).ok()),
    );

    match web_sys::WebTransportError::new_with_message_and_options(&reason, &options) {
        Ok(err) => Error::Session(err),
        Err(err) => Error::from(err),
    }
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
                accept_uni: RefCell::new(None),
                accept_bi: RefCell::new(None),
                recv_datagram: RefCell::new(None),
                send_datagram: RefCell::new(None),
            }),
            accept_uni: Op::default(),
            accept_bi: Op::default(),
            open_uni: Op::default(),
            open_bi: Op::default(),
            send_datagram: Op::default(),
            recv_datagram: Op::default(),
            closed: Op::default(),
        }
    }

    /// Poll for a unidirectional stream created by the peer.
    pub fn poll_accept_uni(&self, cx: &mut Context<'_>) -> Poll<Result<RecvStream, Error>> {
        let inner = &self.shared.inner;
        let reader = match reader(&self.shared.accept_uni, || {
            inner.incoming_unidirectional_streams()
        }) {
            Ok(reader) => reader,
            Err(err) => return Poll::Ready(Err(err)),
        };

        let result = ready!(self.accept_uni.poll(cx, || promise(reader.read())))?;

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
        let reader = match reader(&self.shared.accept_bi, || {
            inner.incoming_bidirectional_streams()
        }) {
            Ok(reader) => reader,
            Err(err) => return Poll::Ready(Err(err)),
        };

        let result = ready!(self.accept_bi.poll(cx, || promise(reader.read())))?;

        match read_value(result) {
            Some(stream) => Poll::Ready(bi_streams(stream)),
            None => Poll::Ready(Err(ready!(self.poll_closed(cx)))),
        }
    }

    /// Poll to open a unidirectional stream, which blocks while there are too many
    /// concurrent streams.
    pub fn poll_open_uni(&self, cx: &mut Context<'_>) -> Poll<Result<SendStream, Error>> {
        let inner = &self.shared.inner;
        let stream = ready!(self
            .open_uni
            .poll(cx, || promise(inner.create_unidirectional_stream())))?;

        Poll::Ready(SendStream::new(stream.unchecked_into()))
    }

    /// Poll to open a bidirectional stream, which blocks while there are too many
    /// concurrent streams.
    pub fn poll_open_bi(
        &self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(SendStream, RecvStream), Error>> {
        let inner = &self.shared.inner;
        let stream = ready!(self
            .open_bi
            .poll(cx, || promise(inner.create_bidirectional_stream())))?;

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
        ready!(self.send_datagram.poll_settled(cx))?;

        match self.try_send_datagram(payload) {
            Ok(()) => Poll::Ready(Ok(())),
            Err(err) => Poll::Ready(Err(err)),
        }
    }

    /// Hand a datagram to the browser without waiting for capacity.
    ///
    /// Delivery was never guaranteed — a datagram can be dropped for congestion,
    /// loss, or size — so there is nothing to report beyond failing to hand it over.
    pub fn try_send_datagram(&self, payload: &[u8]) -> Result<(), Error> {
        let mut slot = self.shared.send_datagram.borrow_mut();
        let writer = match slot.as_ref() {
            Some(writer) => writer,
            None => {
                let writable = datagram_writable(&self.shared.inner.datagrams());
                slot.insert(WritableStreamDefaultWriter::new(&writable)?)
            }
        };

        self.send_datagram
            .start(promise(writer.write_with_chunk(&Uint8Array::from(payload))));

        Ok(())
    }

    /// Poll for a datagram from the network.
    pub fn poll_recv_datagram(&self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Error>> {
        let inner = &self.shared.inner;
        let reader = match reader(&self.shared.recv_datagram, || inner.datagrams().readable()) {
            Ok(reader) => reader,
            Err(err) => return Poll::Ready(Err(err)),
        };

        let result = ready!(self.recv_datagram.poll(cx, || promise(reader.read())))?;

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

        Poll::Ready(session_error(info.unchecked_into()))
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
    pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), Error> {
        poll_fn(|cx| self.poll_open_bi(cx)).await
    }

    /// Creates a new unidirectional stream.
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

impl PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        self.shared.inner == other.shared.inner
    }
}

impl Eq for Session {}
