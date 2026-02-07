mod util;

use std::future::Future;

pub use crate::util::{MaybeSend, MaybeSync};
use bytes::{Buf, BufMut, Bytes, BytesMut};

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

/// A WebTransport Session, able to accept/create streams and send/recv datagrams.
///
/// The session can be cloned to create multiple handles.
/// The session will be closed on drop.
pub trait Session: Clone + MaybeSend + MaybeSync + 'static {
    type SendStream: SendStream;
    type RecvStream: RecvStream;
    type Error: Error;

    /// Block until the peer creates a new unidirectional stream.
    fn accept_uni(&self)
        -> impl Future<Output = Result<Self::RecvStream, Self::Error>> + MaybeSend;

    /// Block until the peer creates a new bidirectional stream.
    fn accept_bi(
        &self,
    ) -> impl Future<Output = Result<(Self::SendStream, Self::RecvStream), Self::Error>> + MaybeSend;

    /// Open a new bidirectional stream, which may block when there are too many concurrent streams.
    fn open_bi(
        &self,
    ) -> impl Future<Output = Result<(Self::SendStream, Self::RecvStream), Self::Error>> + MaybeSend;

    /// Open a new unidirectional stream, which may block when there are too many concurrent streams.
    fn open_uni(&self) -> impl Future<Output = Result<Self::SendStream, Self::Error>> + MaybeSend;

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

    /// Receive a datagram over the network.
    fn recv_datagram(&self) -> impl Future<Output = Result<Bytes, Self::Error>> + MaybeSend;

    /// The maximum size of a datagram that can be sent.
    fn max_datagram_size(&self) -> usize;

    /// Return the negotiated WebTransport subprotocol, if any.
    fn protocol(&self) -> Option<&str> {
        None
    }

    /// Close the connection immediately with a code and reason.
    fn close(&self, code: u32, reason: &str);

    /// Block until the connection is closed by either side.
    fn closed(&self) -> impl Future<Output = Self::Error> + MaybeSend;
}

/// An outgoing stream of bytes to the peer.
///
/// QUIC streams have flow control, which means the send rate is limited by the peer's receive window.
/// The stream will be closed with a graceful FIN when dropped.
pub trait SendStream: MaybeSend {
    type Error: Error;

    /// Write some of the buffer to the stream.
    fn write(&mut self, buf: &[u8])
        -> impl Future<Output = Result<usize, Self::Error>> + MaybeSend;

    /// Write the given buffer to the stream, advancing the internal position.
    fn write_buf<B: Buf + MaybeSend>(
        &mut self,
        buf: &mut B,
    ) -> impl Future<Output = Result<usize, Self::Error>> + MaybeSend {
        async move {
            let chunk = buf.chunk();
            let size = self.write(chunk).await?;
            buf.advance(size);
            Ok(size)
        }
    }

    /// Write the entire [Bytes] chunk to the stream, potentially avoiding a copy.
    fn write_chunk(
        &mut self,
        chunk: Bytes,
    ) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
        async move {
            // Just so the arg isn't mut
            let mut c = chunk;
            self.write_buf(&mut c).await?;
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

    /// Set the stream's priority.
    ///
    /// Streams with lower values will be sent first, but are not guaranteed to arrive first.
    fn set_priority(&mut self, order: u8);

    /// Mark the stream as finished, erroring on any future writes.
    ///
    /// [SendStream::reset] can still be called to abandon any queued data.
    /// [SendStream::closed] should return when the FIN is acknowledged by the peer.
    ///
    /// NOTE: Quinn implicitly calls this on Drop, but it's a common footgun.
    /// Implementations SHOULD [SendStream::reset] on Drop instead.
    fn finish(&mut self) -> Result<(), Self::Error>;

    /// Immediately closes the stream and discards any remaining data.
    ///
    /// This translates into a RESET_STREAM QUIC code.
    /// The peer may not receive the reset code if the stream is already closed.
    fn reset(&mut self, code: u32);

    /// Block until the stream is closed by either side.
    ///
    /// This includes:
    /// - We sent a RESET_STREAM via [SendStream::reset]
    /// - We received a STOP_SENDING via [RecvStream::stop]
    /// - A FIN is acknowledged by the peer via [SendStream::finish]
    ///
    /// Some implementations do not support FIN acknowledgement, in which case this will block until the FIN is sent.
    ///
    /// NOTE: This takes a &mut to match Quinn and to simplify the implementation.
    fn closed(&mut self) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend;
}

/// An incoming stream of bytes from the peer.
///
/// All bytes are flushed in order and the stream is flow controlled.
/// The stream will be closed with STOP_SENDING code=0 when dropped.
pub trait RecvStream: MaybeSend {
    type Error: Error;

    /// Read the next chunk of data, up to the max size.
    ///
    /// This returns a chunk of data instead of copying, which may be more efficient.
    fn read(
        &mut self,
        dst: &mut [u8],
    ) -> impl Future<Output = Result<Option<usize>, Self::Error>> + MaybeSend;

    /// Read some data into the provided buffer.
    ///
    /// The number of bytes read is returned, or None if the stream is closed.
    /// The buffer will be advanced by the number of bytes read.
    fn read_buf<B: BufMut + MaybeSend>(
        &mut self,
        buf: &mut B,
    ) -> impl Future<Output = Result<Option<usize>, Self::Error>> + MaybeSend {
        async move {
            let dst = unsafe {
                std::mem::transmute::<&mut bytes::buf::UninitSlice, &mut [u8]>(buf.chunk_mut())
            };
            let size = match self.read(dst).await? {
                Some(size) if size > 0 => size,
                _ => return Ok(None),
            };

            unsafe { buf.advance_mut(size) };

            Ok(Some(size))
        }
    }

    /// Read the next chunk of data, up to the max size.
    ///
    /// This returns a chunk of data instead of copying, which may be more efficient.
    fn read_chunk(
        &mut self,
        max: usize,
    ) -> impl Future<Output = Result<Option<Bytes>, Self::Error>> + MaybeSend {
        async move {
            // Don't allocate too much. Write your own if you want to increase this buffer.
            let mut buf = BytesMut::with_capacity(max.min(8 * 1024));

            // TODO Test this, I think it will work?
            Ok(self.read_buf(&mut buf).await?.map(|_| buf.freeze()))
        }
    }

    /// Send a `STOP_SENDING` QUIC code, informing the peer that no more data will be read.
    ///
    /// An implementation MUST do this on Drop otherwise flow control will be leaked.
    /// Call this method manually if you want to specify a code yourself.
    fn stop(&mut self, code: u32);

    /// Block until the stream has been closed by either side.
    ///
    /// This includes:
    /// - We received a RESET_STREAM via [SendStream::reset]
    /// - We sent a STOP_SENDING via [RecvStream::stop]
    /// - We received a FIN via [SendStream::finish] and read all data.
    fn closed(&mut self) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend;

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
