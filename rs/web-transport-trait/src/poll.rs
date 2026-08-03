//! The `poll`-based surface of a WebTransport session and its streams.
//!
//! These traits are the sans-I/O half of this crate. They describe a transport as a
//! state machine that a caller steps from its own loop, rather than a set of futures
//! that a runtime drives.
//!
//! Two properties are deliberate, and both are constraints on what an implementation
//! is *allowed* to be rather than features it gets:
//!
//! - **No `Send` or `Sync` bound.** A transport pinned to one thread — a
//!   thread-per-core `io_uring` runtime, say — can implement these. The async traits
//!   in the crate root add `MaybeSend` on top, because their futures need it; the
//!   poll traits do not, so a `!Send` stack stays expressible all the way down.
//!   [`Session`]'s associated stream types only require the poll halves, so the
//!   bound cannot leak back in through them.
//!
//! - **`&mut self` throughout, and no `Clone`.** A sans-I/O state machine owns its
//!   state and mutates it in place. A `&self` surface would force every
//!   implementation into interior mutability — an `Arc<Mutex<..>>` around the
//!   connection, or a slot shared between callers — whether or not its own design
//!   needs one. Taking `&mut self` gives each handle exactly one owner by
//!   construction, which is also what makes retained in-progress state safe: a
//!   `poll_open_uni` that claimed stream credit before returning [`Poll::Pending`]
//!   has an unambiguous owner to resume it.
//!
//! Run operations concurrently by cloning the concrete session, where each clone
//! gets its own state. `Clone` is deliberately *not* a supertrait, so a session that
//! cannot be duplicated is still expressible.
//!
//! # Retaining state between calls
//!
//! A `poll_*` method may keep its own progress across calls, and often should —
//! [`SendStream::poll_write_buf`] will typically have reserved send capacity by
//! the time it returns [`Poll::Pending`], and starting over would give that back.
//! What it must not keep is anything belonging to the *caller*:
//!
//! - Nothing about a buffer argument may be assumed to survive. Every call gets a
//!   fresh one, and a caller is free to retry a pending write with a shorter buffer.
//!   Retained progress must be reconciled against the buffer actually presented, not
//!   the one that started the operation.
//! - Whatever ends the stream — [`SendStream::reset`],
//!   [`SendStream::finish`], a peer STOP_SENDING seen by
//!   [`SendStream::poll_closed`] — must release retained progress. Nothing else
//!   will: the guards at the top of a write return early once the stream is closed,
//!   so no later call reaches the cleanup, and a reservation held past that point is
//!   leaked for the life of the stream.
//! - A resource that other handles contend for — a queue slot, a shared lock —
//!   should not be held across a wait. A caller may abandon an operation simply by
//!   never polling it again, and anything the retained state owns is abandoned with
//!   it. Retain the *waiting*, not the resource.

use std::task::{ready, Context, Poll};

use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::{Error, Stats};

/// The stream pair produced by opening or accepting a bidirectional stream.
pub type BiStreams<S> = (<S as Session>::SendStream, <S as Session>::RecvStream);

/// The `poll`-based surface of a WebTransport session.
///
/// See the [module docs](self) for why this takes `&mut self` and carries no `Send`
/// bound.
pub trait Session {
    /// The outgoing stream type. Only the poll half is required, so a `!Send`
    /// session can hang `!Send` streams off it.
    type SendStream: SendStream;

    /// The incoming stream type. Only the poll half is required, so a `!Send`
    /// session can hang `!Send` streams off it.
    type RecvStream: RecvStream;

    /// The error type for every operation on this session.
    type Error: Error;

    /// Poll for a unidirectional stream created by the peer.
    fn poll_accept_uni(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::RecvStream, Self::Error>>;

    /// Poll for a bidirectional stream created by the peer.
    fn poll_accept_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<BiStreams<Self>, Self::Error>>;

    /// Poll to open a unidirectional stream, which blocks while there are too many
    /// concurrent streams.
    fn poll_open_uni(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::SendStream, Self::Error>>;

    /// Poll to open a bidirectional stream, which blocks while there are too many
    /// concurrent streams.
    fn poll_open_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<BiStreams<Self>, Self::Error>>;

    /// Poll to send a datagram over the network.
    ///
    /// Returns [`Poll::Pending`] while the transport has no room for it, so a caller
    /// can wait for capacity rather than having the payload dropped underneath it.
    ///
    /// `payload` is taken by reference, not by value or as a [`Buf`]: a
    /// [`Poll::Pending`] return means the caller retries with the same datagram, and
    /// both of those would have consumed it. (A datagram also needs *contiguous*
    /// bytes, and the only way to get those from a generic [`Buf`] is
    /// [`Buf::copy_to_bytes`], which consumes.)
    ///
    /// Accepting a datagram is not delivery. QUIC datagrams may still be dropped:
    /// - Network congestion.
    /// - Random packet loss.
    /// - Payload is larger than `max_datagram_size()`
    /// - Peer is not receiving datagrams.
    /// - ???
    fn poll_send_datagram(
        &mut self,
        cx: &mut Context<'_>,
        payload: &[u8],
    ) -> Poll<Result<(), Self::Error>>;

    /// Poll for a datagram from the network.
    fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>>;

    /// The maximum size of a datagram that can be sent.
    fn max_datagram_size(&self) -> usize;

    /// Return the application protocol negotiated for this session, if any.
    ///
    /// For WebTransport over HTTP/3 this is the selected WebTransport subprotocol;
    /// for raw QUIC it is the negotiated ALPN. Return `None` if the transport does
    /// not negotiate either. This is required rather than defaulted: a transport
    /// that negotiates an application protocol and forgets to report it is a silent
    /// bug, and the default hid that.
    fn protocol(&self) -> Option<&str>;

    /// Close the connection immediately with a code and reason.
    ///
    /// Idempotent, and deliberately infallible: closing an already-closed connection
    /// achieved what the caller asked for, and there is nothing they could do with an
    /// error.
    fn close(&mut self, code: u32, reason: &str);

    /// Poll until the connection is closed by either side.
    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Self::Error>;

    /// Return connection-level statistics.
    ///
    /// Return [`crate::StatsUnavailable`] if the transport does not track them. Required
    /// rather than defaulted for the same reason as [`protocol`](Self::protocol).
    fn stats(&self) -> impl Stats;
}

/// The `poll`-based surface of an outgoing stream.
///
/// See the [module docs](self) for what a `poll_*` method may retain between calls.
pub trait SendStream {
    /// The error type for every operation on this stream.
    type Error: Error;

    /// Poll to write some of the buffer to the stream, returning how many bytes were
    /// written. See [`poll_write_buf`](Self::poll_write_buf) for the partial-write
    /// contract, which this shares.
    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>>;

    /// Poll to write some of the given buffer to the stream, advancing it by the
    /// number of bytes written. This may be less than the whole buffer, so callers
    /// loop.
    ///
    /// # Partial writes
    ///
    /// Implementations must not advance `buf` past the bytes they accepted for
    /// sending. (Whether those bytes reach the peer is a separate matter — a reset
    /// or a dead connection can still discard accepted bytes.) A returned
    /// [`Poll::Pending`] must leave `buf` exactly where the accepted bytes end.
    /// Callers race writes against other work, so a byte taken from `buf` but never
    /// accepted becomes a silent hole in the stream, which the peer decodes as a
    /// truncated or garbage frame. Wait for send capacity *before* consuming from
    /// `buf`, never after.
    ///
    /// Override this to avoid a copy when the underlying transport can take
    /// ownership of `buf`'s bytes — see [`Buf::copy_to_bytes`], which is free for a
    /// [`Bytes`] source.
    fn poll_write_buf<B: Buf>(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<Result<usize, Self::Error>> {
        let size = ready!(self.poll_write(cx, buf.chunk()))?;
        buf.advance(size);
        Poll::Ready(Ok(size))
    }

    /// Set the stream's priority.
    ///
    /// Streams with higher values will be sent first, but are not guaranteed to
    /// arrive first. This matches the W3C WebTransport `sendOrder` convention (and
    /// quinn's scheduler).
    fn set_priority(&mut self, order: u8);

    /// Mark the stream as finished, erroring on any future writes.
    ///
    /// [`reset`](Self::reset) can still be called to abandon any queued data.
    /// [`poll_closed`](Self::poll_closed) should resolve when the FIN is acknowledged
    /// by the peer.
    ///
    /// NOTE: Quinn implicitly calls this on Drop, but it's a common footgun.
    /// Implementations SHOULD [`reset`](Self::reset) on Drop instead.
    fn finish(&mut self) -> Result<(), Self::Error>;

    /// Immediately closes the stream and discards any remaining data.
    ///
    /// This translates into a RESET_STREAM QUIC code.
    /// The peer may not receive the reset code if the stream is already closed.
    ///
    /// Takes `&mut self` rather than `self` even though it is terminal, so a caller
    /// can still [`poll_closed`](Self::poll_closed) afterwards to await the peer —
    /// and so it matches [`finish`](Self::finish), which must not consume the stream
    /// for exactly that reason.
    fn reset(&mut self, code: u32);

    /// Poll until the stream is closed by either side.
    ///
    /// This includes:
    /// - We sent a RESET_STREAM via [`reset`](Self::reset)
    /// - We received a STOP_SENDING via [`RecvStream::stop`]
    /// - A FIN is acknowledged by the peer via [`finish`](Self::finish)
    ///
    /// Some implementations do not support FIN acknowledgement, in which case this
    /// resolves once the FIN is sent.
    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>>;
}

/// The `poll`-based surface of an incoming stream.
///
/// See the [module docs](self) for what a `poll_*` method may retain between calls.
pub trait RecvStream {
    /// The error type for every operation on this stream.
    type Error: Error;

    /// Poll to read some data into the provided slice.
    ///
    /// Returns the number of bytes read, or `None` once the peer has finished the
    /// stream. An empty `dst` reads nothing and returns `Some(0)` — asking for no
    /// bytes is not end of stream.
    fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, Self::Error>>;

    /// Poll to read some data into the provided buffer, advancing it by the number
    /// of bytes read.
    ///
    /// Override this to avoid a copy when the underlying transport already owns the
    /// bytes as a [`Bytes`], which can be handed to [`BufMut::put`] directly.
    fn poll_read_buf<B: BufMut>(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<Result<Option<usize>, Self::Error>> {
        let len = buf.chunk_mut().len();

        // A destination with no room is not a closed stream. Collapsing the two
        // would turn "buffer full" into "stream ended", which reads as truncation.
        if len == 0 {
            return Poll::Ready(Ok(Some(0)));
        }

        let dst = unsafe {
            std::mem::transmute::<&mut bytes::buf::UninitSlice, &mut [u8]>(buf.chunk_mut())
        };

        let size = match ready!(self.poll_read(cx, dst))? {
            Some(size) if size > 0 => size,
            Some(_) => return Poll::Ready(Ok(Some(0))),
            None => return Poll::Ready(Ok(None)),
        };

        unsafe { buf.advance_mut(size) };

        Poll::Ready(Ok(Some(size)))
    }

    /// Poll for the next chunk of data, up to `max` bytes.
    ///
    /// Override this when the transport can hand over a [`Bytes`] it already owns;
    /// the default allocates and copies.
    fn poll_read_chunk(
        &mut self,
        cx: &mut Context<'_>,
        max: usize,
    ) -> Poll<Result<Option<Bytes>, Self::Error>> {
        // As in `poll_read_buf`: asking for nothing is not end of stream.
        if max == 0 {
            return Poll::Ready(Ok(Some(Bytes::new())));
        }

        // Don't allocate too much. Override this to avoid the copy, or to use a
        // larger per-poll buffer.
        let capacity = max.min(8 * 1024);
        let mut buf = BytesMut::with_capacity(capacity);

        // Slice to `capacity` rather than trusting the allocation: `with_capacity`
        // promises only a lower bound, so an over-allocation would let this return
        // more than `max`, which the method documents it won't.
        let dst = unsafe {
            std::mem::transmute::<&mut bytes::buf::UninitSlice, &mut [u8]>(buf.chunk_mut())
        };
        let dst = &mut dst[..capacity];

        let size = match ready!(self.poll_read(cx, dst))? {
            Some(size) if size > 0 => size,
            Some(_) => return Poll::Ready(Ok(Some(Bytes::new()))),
            None => return Poll::Ready(Ok(None)),
        };

        // The read wrote into spare capacity, so the length is still zero.
        unsafe { buf.advance_mut(size) };

        Poll::Ready(Ok(Some(buf.freeze())))
    }

    /// Send a `STOP_SENDING` QUIC code, informing the peer that no more data will be
    /// read.
    ///
    /// An implementation MUST do this on Drop otherwise flow control will be leaked.
    /// Call this method manually if you want to specify a code yourself.
    fn stop(&mut self, code: u32);

    /// Poll until the stream has been closed by either side.
    ///
    /// This includes:
    /// - We received a RESET_STREAM via [`SendStream::reset`]
    /// - We sent a STOP_SENDING via [`stop`](Self::stop)
    /// - We received a FIN via [`SendStream::finish`] and read all data.
    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>>;
}
