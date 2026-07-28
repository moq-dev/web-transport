mod util;

use std::{
    future::{poll_fn, Future},
    task::{ready, Context, Poll},
    time::Duration,
};

pub use crate::util::{MaybeSend, MaybeSync, OpState, SessionOps};
use bytes::{Buf, BufMut, Bytes, BytesMut};

/// Connection-level statistics.
///
/// Methods return `Option` — `None` means the implementation doesn't track
/// this metric, while `Some(0)` means actually zero.
pub trait Stats {
    /// Total bytes sent over the connection, including retransmissions and overhead.
    fn bytes_sent(&self) -> Option<u64> {
        None
    }

    /// Total bytes received over the connection, including duplicate and overhead.
    fn bytes_received(&self) -> Option<u64> {
        None
    }

    /// Total bytes lost (detected via retransmission or acknowledgement).
    fn bytes_lost(&self) -> Option<u64> {
        None
    }

    /// Total number of datagrams sent.
    fn packets_sent(&self) -> Option<u64> {
        None
    }

    /// Total number of datagrams received.
    fn packets_received(&self) -> Option<u64> {
        None
    }

    /// Total number of datagrams detected as lost.
    fn packets_lost(&self) -> Option<u64> {
        None
    }

    /// Smoothed round-trip time estimate.
    fn rtt(&self) -> Option<Duration> {
        None
    }

    /// Estimated available send bandwidth, in bits per second.
    fn estimated_send_rate(&self) -> Option<u64> {
        None
    }
}

/// Default stats implementation that returns `None` for all metrics.
pub struct StatsUnavailable;
impl Stats for StatsUnavailable {}

/// Error trait for WebTransport operations.
///
/// Implementations must be Send + Sync + 'static for use across async boundaries.
pub trait Error: std::error::Error + MaybeSend + MaybeSync + 'static {
    /// Returns the error code and reason if this was an application error.
    ///
    /// NOTE: Reason reasons are technically bytes on the wire, but we convert to a String for convenience.
    fn session_error(&self) -> Option<(u32, String)>;

    /// Returns the error code if this was a stream error.
    fn stream_error(&self) -> Option<u32> {
        None
    }
}

/// The stream pair produced by opening or accepting a bidirectional stream.
pub type BiStreams<S> = (
    <S as PollSession>::SendStream,
    <S as PollSession>::RecvStream,
);

/// The `poll`-based half of a WebTransport session.
///
/// This trait carries no `Send` bound, like [`PollSendStream`] and
/// [`PollRecvStream`]. A transport pinned to one thread can implement it and be
/// driven from a poll loop; it just won't get [`Session`]'s async conveniences,
/// which need `Send` futures.
///
/// # `&mut self`
///
/// Operations take `&mut self` so each handle has exactly one owner, which is what
/// makes a retained in-progress operation safe. `poll_open_uni` claims stream
/// credit before it resolves, so a `Pending` poll has to resume rather than start
/// over — and the state it resumes needs an unambiguous owner.
///
/// A `&self` poll method holding retained state would have to either share one
/// slot between concurrent callers — where the second polls the first's future
/// with its own waker, so the first hangs and the resolved stream goes to the
/// wrong task — or clone the session on every call. Neither is necessary here.
///
/// Run operations concurrently by cloning the session; each clone gets its own
/// poll state. `Clone` is deliberately not required, so a session that cannot be
/// duplicated is still expressible.
pub trait PollSession {
    type SendStream: PollSendStream + 'static;
    type RecvStream: PollRecvStream + 'static;
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

    /// Send a datagram over the network.
    ///
    /// QUIC datagrams may be dropped for any reason:
    /// - Network congestion.
    /// - Random packet loss.
    /// - Payload is larger than `max_datagram_size()`
    /// - Peer is not receiving datagrams.
    /// - Peer has too many outstanding datagrams.
    /// - ???
    fn send_datagram(&self, payload: Bytes) -> Result<(), Self::Error>;

    /// Poll for a datagram from the network.
    fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>>;

    /// The maximum size of a datagram that can be sent.
    fn max_datagram_size(&self) -> usize;

    /// Return the negotiated WebTransport subprotocol, if any.
    fn protocol(&self) -> Option<&str> {
        None
    }

    /// Close the connection immediately with a code and reason.
    fn close(&self, code: u32, reason: &str);

    /// Poll until the connection is closed by either side.
    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Self::Error>;

    /// Return connection-level statistics, if supported.
    fn stats(&self) -> impl Stats {
        StatsUnavailable
    }
}

/// A WebTransport Session, able to accept/create streams and send/recv datagrams.
///
/// The session will be closed on drop.
///
/// Every method here is a provided method over [`PollSession`], so a backend
/// implements the poll surface and then opts in with an empty
/// `impl Session for MySession {}`.
pub trait Session: PollSession + MaybeSend + MaybeSync + 'static
where
    Self::SendStream: SendStream,
    Self::RecvStream: RecvStream,
{
    /// Block until the peer creates a new unidirectional stream.
    fn accept_uni(
        &mut self,
    ) -> impl Future<Output = Result<Self::RecvStream, Self::Error>> + MaybeSend {
        poll_fn(|cx| self.poll_accept_uni(cx))
    }

    /// Block until the peer creates a new bidirectional stream.
    fn accept_bi(
        &mut self,
    ) -> impl Future<Output = Result<BiStreams<Self>, Self::Error>> + MaybeSend {
        poll_fn(|cx| self.poll_accept_bi(cx))
    }

    /// Open a new unidirectional stream, which may block when there are too many concurrent streams.
    fn open_uni(
        &mut self,
    ) -> impl Future<Output = Result<Self::SendStream, Self::Error>> + MaybeSend {
        poll_fn(|cx| self.poll_open_uni(cx))
    }

    /// Open a new bidirectional stream, which may block when there are too many concurrent streams.
    fn open_bi(
        &mut self,
    ) -> impl Future<Output = Result<BiStreams<Self>, Self::Error>> + MaybeSend {
        poll_fn(|cx| self.poll_open_bi(cx))
    }

    /// Receive a datagram over the network.
    fn recv_datagram(&mut self) -> impl Future<Output = Result<Bytes, Self::Error>> + MaybeSend {
        poll_fn(|cx| self.poll_recv_datagram(cx))
    }

    /// Block until the connection is closed by either side.
    fn closed(&mut self) -> impl Future<Output = Self::Error> + MaybeSend {
        poll_fn(|cx| self.poll_closed(cx))
    }
}

/// The `poll`-based half of an outgoing stream.
///
/// This trait carries no `Send` bound. A transport whose streams are pinned to one
/// thread — a thread-per-core `io_uring` runtime, say — can implement it and be
/// driven from a poll loop; it just won't get [`SendStream`]'s async conveniences,
/// which need `Send` futures.
///
/// # Retaining state between calls
///
/// A `poll_*` method may keep its own progress across calls, and often should —
/// [`poll_write_buf`](Self::poll_write_buf) will typically have reserved send
/// capacity by the time it returns [`Poll::Pending`], and starting over would give
/// that back. What it must not keep is anything belonging to the *caller*:
///
/// - Nothing about `buf` may be assumed to survive. Every call gets a fresh
///   argument, and a caller is free to retry a pending write with a shorter buffer.
///   Retained progress must be reconciled against the buffer actually presented,
///   not the one that started the operation.
/// - Whatever ends the stream — [`reset`](Self::reset), [`finish`](Self::finish), a
///   peer STOP_SENDING seen by [`poll_closed`](Self::poll_closed) — must release
///   retained progress. Nothing else will: the guards at the top of a write return
///   early once the stream is closed, so no later call reaches the cleanup, and a
///   reservation held past that point is leaked for the life of the stream.
pub trait PollSendStream {
    type Error: Error;

    /// Poll to write some of the buffer to the stream, returning how many bytes
    /// were written. See [`poll_write_buf`](Self::poll_write_buf) for the
    /// partial-write contract, which this shares.
    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>>;

    /// Poll to write some of the given buffer to the stream, advancing it by the
    /// number of bytes written. This may be less than the whole buffer, so callers
    /// loop (or use [`SendStream::write_all_buf`]).
    ///
    /// # Partial writes
    ///
    /// Implementations must not advance `buf` past the bytes they accepted for
    /// sending. (Whether those bytes reach the peer is a separate matter — a reset
    /// or a dead connection can still discard accepted bytes.) A returned
    /// [`Poll::Pending`], or a dropped [`SendStream::write_buf`] future, must leave
    /// `buf` exactly where the accepted bytes end. Callers race writes against other
    /// work, so a byte taken from `buf` but never accepted becomes a silent hole in
    /// the stream, which the peer decodes as a truncated or garbage frame. Wait for
    /// send capacity *before* consuming from `buf`, never after.
    ///
    /// Override this to avoid a copy when the underlying transport can take
    /// ownership of `buf`'s bytes — see [`Buf::copy_to_bytes`], which is free for
    /// a [`Bytes`] source.
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
    /// Streams with higher values will be sent first, but are not guaranteed to arrive first.
    /// This matches the W3C WebTransport `sendOrder` convention (and quinn's scheduler).
    fn set_priority(&mut self, order: u8);

    /// Mark the stream as finished, erroring on any future writes.
    ///
    /// [PollSendStream::reset] can still be called to abandon any queued data.
    /// [PollSendStream::poll_closed] should resolve when the FIN is acknowledged by the peer.
    ///
    /// NOTE: Quinn implicitly calls this on Drop, but it's a common footgun.
    /// Implementations SHOULD [PollSendStream::reset] on Drop instead.
    fn finish(&mut self) -> Result<(), Self::Error>;

    /// Immediately closes the stream and discards any remaining data.
    ///
    /// This translates into a RESET_STREAM QUIC code.
    /// The peer may not receive the reset code if the stream is already closed.
    fn reset(&mut self, code: u32);

    /// Poll until the stream is closed by either side.
    ///
    /// This includes:
    /// - We sent a RESET_STREAM via [PollSendStream::reset]
    /// - We received a STOP_SENDING via [PollRecvStream::stop]
    /// - A FIN is acknowledged by the peer via [PollSendStream::finish]
    ///
    /// Some implementations do not support FIN acknowledgement, in which case this will block until the FIN is sent.
    ///
    /// NOTE: This takes a &mut to match Quinn and to simplify the implementation.
    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>>;
}

/// An outgoing stream of bytes to the peer.
///
/// QUIC streams have flow control, which means the send rate is limited by the peer's receive window.
/// The stream will be closed with a graceful FIN when dropped.
///
/// Every method here is a provided method over [`PollSendStream`], so a backend
/// implements the poll surface and then opts in with an empty `impl SendStream for
/// MyStream {}`. Overriding is still allowed, and is how a transport that can take
/// ownership of a [`Bytes`] keeps [`write_chunk`](Self::write_chunk) zero-copy.
pub trait SendStream: PollSendStream + MaybeSend {
    /// Write some of the buffer to the stream, returning how many bytes were
    /// written.
    fn write(
        &mut self,
        buf: &[u8],
    ) -> impl Future<Output = Result<usize, Self::Error>> + MaybeSend {
        poll_fn(move |cx| self.poll_write(cx, buf))
    }

    /// Write some of the given buffer to the stream, advancing it by the number of
    /// bytes written. This may be less than the whole buffer, so callers loop (or
    /// use [`write_all_buf`](Self::write_all_buf)).
    ///
    /// See [`PollSendStream::poll_write_buf`] for the partial-write contract.
    fn write_buf<B: Buf + MaybeSend>(
        &mut self,
        buf: &mut B,
    ) -> impl Future<Output = Result<usize, Self::Error>> + MaybeSend {
        poll_fn(move |cx| self.poll_write_buf(cx, buf))
    }

    /// Write the entire [Bytes] chunk to the stream, potentially avoiding a copy.
    fn write_chunk(
        &mut self,
        chunk: Bytes,
    ) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
        async move {
            // Just so the arg isn't mut
            let mut c = chunk;
            self.write_all_buf(&mut c).await?;
            Ok(())
        }
    }

    /// A helper to write all the data in the buffer.
    fn write_all(
        &mut self,
        buf: &[u8],
    ) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
        async move {
            let mut pos = 0;
            while pos < buf.len() {
                pos += self.write(&buf[pos..]).await?;
            }
            Ok(())
        }
    }

    /// A helper to write all of the data in the buffer.
    fn write_all_buf<B: Buf + MaybeSend>(
        &mut self,
        buf: &mut B,
    ) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
        async move {
            while buf.has_remaining() {
                self.write_buf(buf).await?;
            }
            Ok(())
        }
    }

    /// Block until the stream is closed by either side.
    ///
    /// See [`PollSendStream::poll_closed`] for what counts as closed.
    fn closed(&mut self) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
        poll_fn(|cx| self.poll_closed(cx))
    }
}

/// The `poll`-based half of an incoming stream.
///
/// Like [`PollSendStream`], this carries no `Send` bound, so a thread-pinned
/// transport can implement it. See [`PollSendStream`] for the rules about what a
/// `poll_*` method may retain between calls.
pub trait PollRecvStream {
    type Error: Error;

    /// Poll to read some data into the provided slice.
    ///
    /// The number of bytes read is returned, or None if the stream is closed.
    fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, Self::Error>>;

    /// Poll to read some data into the provided buffer.
    ///
    /// The number of bytes read is returned, or None if the stream is closed.
    /// The buffer will be advanced by the number of bytes read.
    ///
    /// Override this to avoid a copy when the underlying transport already owns
    /// the bytes as a [`Bytes`], which can be handed to [`BufMut::put`] directly.
    fn poll_read_buf<B: BufMut>(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<Result<Option<usize>, Self::Error>> {
        let dst = buf.chunk_mut();
        let len = dst.len();

        // A destination with no room is not a closed stream. Backends already draw
        // that distinction — they report `Some(0)` for an empty slice rather than
        // `None` — and collapsing it here would turn "buffer full" into "stream
        // ended", which reads as truncation to the caller.
        if len == 0 {
            return Poll::Ready(Ok(Some(0)));
        }

        // `poll_read` takes an initialized slice, and `chunk_mut` hands out memory
        // that may not be. Zero it: transmuting uninitialized bytes into a
        // `&mut [u8]` is undefined behaviour whether or not the callee reads them.
        // Override this method to skip the memset.
        //
        // SAFETY: the pointer is valid for `len` bytes, and once written every one
        // of them is initialized, so the slice refers to initialized memory.
        let dst = unsafe {
            let ptr = dst.as_mut_ptr();
            std::ptr::write_bytes(ptr, 0, len);
            std::slice::from_raw_parts_mut(ptr, len)
        };

        let size = match ready!(self.poll_read(cx, dst))? {
            Some(size) if size > 0 => size,
            _ => return Poll::Ready(Ok(None)),
        };

        // `advance_mut` is unsafe and `PollRecvStream` is not, so a wrong count
        // here must not become unsoundness. Panic rather than clamp: an
        // implementation that reports more than the buffer it was handed is
        // broken, and quietly trusting a smaller number would hide it.
        //
        // A count that is wrong but in range stays a plain logic bug — the caller
        // sees the zeroed bytes, which is the usual outcome for a `read` that
        // misreports.
        assert!(
            size <= len,
            "poll_read reported {size} bytes for a {len}-byte buffer"
        );

        // SAFETY: `size` is within `dst`, and every byte of `dst` was initialized
        // above, so `size` bytes of the chunk are initialized.
        unsafe { buf.advance_mut(size) };

        Poll::Ready(Ok(Some(size)))
    }

    /// Poll for the next chunk of data, up to the max size.
    ///
    /// Override this when the transport can hand over a [`Bytes`] it already
    /// owns; the default allocates and copies.
    fn poll_read_chunk(
        &mut self,
        cx: &mut Context<'_>,
        max: usize,
    ) -> Poll<Result<Option<Bytes>, Self::Error>> {
        // Don't allocate too much. Override this to avoid the copy, or to use a
        // larger per-poll buffer.
        //
        // Zeroed rather than `with_capacity`: `poll_read` needs an initialized
        // slice, and a buffer sized exactly to `max` also bounds the read without
        // depending on the allocation being exact — `with_capacity` promises only a
        // lower bound.
        // As in `poll_read_buf`: asking for nothing is not end of stream.
        if max == 0 {
            return Poll::Ready(Ok(Some(Bytes::new())));
        }

        let mut buf = BytesMut::zeroed(max.min(8 * 1024));

        let size = match ready!(self.poll_read(cx, &mut buf))? {
            Some(size) if size > 0 => size,
            _ => return Poll::Ready(Ok(None)),
        };
        buf.truncate(size);

        Poll::Ready(Ok(Some(buf.freeze())))
    }

    /// Send a `STOP_SENDING` QUIC code, informing the peer that no more data will be read.
    ///
    /// An implementation MUST do this on Drop otherwise flow control will be leaked.
    /// Call this method manually if you want to specify a code yourself.
    fn stop(&mut self, code: u32);

    /// Poll until the stream has been closed by either side.
    ///
    /// This includes:
    /// - We received a RESET_STREAM via [PollSendStream::reset]
    /// - We sent a STOP_SENDING via [PollRecvStream::stop]
    /// - We received a FIN via [PollSendStream::finish] and read all data.
    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>>;
}

/// An incoming stream of bytes from the peer.
///
/// All bytes are flushed in order and the stream is flow controlled.
/// The stream will be closed with STOP_SENDING code=0 when dropped.
///
/// Every method here is a provided method over [`PollRecvStream`], so a backend
/// implements the poll surface and then opts in with an empty `impl RecvStream for
/// MyStream {}`.
pub trait RecvStream: PollRecvStream + MaybeSend {
    /// Read some data into the provided slice.
    ///
    /// The number of bytes read is returned, or None if the stream is closed.
    fn read(
        &mut self,
        dst: &mut [u8],
    ) -> impl Future<Output = Result<Option<usize>, Self::Error>> + MaybeSend {
        poll_fn(move |cx| self.poll_read(cx, dst))
    }

    /// Read some data into the provided buffer.
    ///
    /// The number of bytes read is returned, or None if the stream is closed.
    /// The buffer will be advanced by the number of bytes read.
    fn read_buf<B: BufMut + MaybeSend>(
        &mut self,
        buf: &mut B,
    ) -> impl Future<Output = Result<Option<usize>, Self::Error>> + MaybeSend {
        poll_fn(move |cx| self.poll_read_buf(cx, buf))
    }

    /// Read the next chunk of data, up to the max size.
    ///
    /// This returns a chunk of data instead of copying, which may be more efficient.
    fn read_chunk(
        &mut self,
        max: usize,
    ) -> impl Future<Output = Result<Option<Bytes>, Self::Error>> + MaybeSend {
        poll_fn(move |cx| self.poll_read_chunk(cx, max))
    }

    /// Block until the stream has been closed by either side.
    ///
    /// See [`PollRecvStream::poll_closed`] for what counts as closed.
    fn closed(&mut self) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
        poll_fn(|cx| self.poll_closed(cx))
    }

    /// A helper to keep reading until the stream is closed.
    fn read_all(&mut self) -> impl Future<Output = Result<Bytes, Self::Error>> + MaybeSend {
        async move {
            let mut buf = BytesMut::new();
            self.read_all_buf(&mut buf).await?;
            Ok(buf.freeze())
        }
    }

    /// A helper to keep reading until the buffer is full.
    fn read_all_buf<B: BufMut + MaybeSend>(
        &mut self,
        buf: &mut B,
    ) -> impl Future<Output = Result<usize, Self::Error>> + MaybeSend {
        async move {
            let mut size = 0;
            while buf.has_remaining_mut() {
                match self.read_buf(buf).await? {
                    Some(n) if n > 0 => size += n,
                    _ => break,
                }
            }
            Ok(size)
        }
    }
}
