use futures::ready;
use std::{
    collections::VecDeque,
    future::poll_fn,
    io,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use tokio_quiche::quiche;

use bytes::{BufMut, Bytes, BytesMut};
use tokio::io::{AsyncRead, ReadBuf};

use crate::ez::DriverState;

use super::{Lock, StreamError, StreamId};

use tokio_quiche::quic::QuicheConnection;

// "recvdrop" in ascii; if you see this then read everything or close(code)
// decimal: 7305813194079104880
const DROP_CODE: u64 = 0x6563766464726F70;

pub(super) struct RecvState {
    id: StreamId,

    // Data that has been read and needs to be returned to the application.
    queued: VecDeque<Bytes>,

    // The amount of data that should be queued.
    max: usize,

    // The driver wakes up the application when data is available.
    blocked: Option<Waker>,

    // Set when STREAM_FIN
    fin: bool,

    // Set when RESET_STREAM is received
    reset: Option<u64>,

    // Set when STOP_SENDING is sent
    stop: Option<u64>,

    // Buffer for reading data.
    buf: BytesMut,

    // The size of the buffer doubles each time until it reaches the maximum size.
    buf_capacity: usize,

    // Set when FIN is received, STOP_SENDING is sent, or RESET_STREAM is received.
    closed: bool,
}

impl RecvState {
    pub fn new(id: StreamId) -> Self {
        Self {
            id,
            queued: Default::default(),
            max: 0,
            blocked: None,
            fin: false,
            reset: None,
            stop: None,
            buf: BytesMut::with_capacity(64),
            buf_capacity: 64,
            closed: false,
        }
    }

    pub fn poll_read_chunk(
        &mut self,
        waker: &Waker,
        max: usize,
    ) -> Poll<Result<Option<Bytes>, StreamError>> {
        if let Some(reset) = self.reset {
            return Poll::Ready(Err(StreamError::Reset(reset)));
        }

        if let Some(stop) = self.stop {
            return Poll::Ready(Err(StreamError::Stop(stop)));
        }

        if let Some(mut chunk) = self.queued.pop_front() {
            if chunk.len() > max {
                let remain = chunk.split_off(max);
                self.queued.push_front(remain);
            }
            return Poll::Ready(Ok(Some(chunk)));
        }

        if self.fin {
            return Poll::Ready(Ok(None));
        }

        // We'll return None if FIN, otherwise return an empty chunk.
        if max == 0 {
            return Poll::Ready(Ok(Some(Bytes::new())));
        }

        self.max = max;
        self.blocked = Some(waker.clone());

        Poll::Pending
    }

    pub fn poll_closed(&mut self, waker: &Waker) -> Poll<Result<(), StreamError>> {
        if self.fin && self.queued.is_empty() {
            Poll::Ready(Ok(()))
        } else if let Some(reset) = self.reset {
            Poll::Ready(Err(StreamError::Reset(reset)))
        } else if let Some(stop) = self.stop {
            Poll::Ready(Err(StreamError::Stop(stop)))
        } else {
            self.blocked = Some(waker.clone());
            Poll::Pending
        }
    }

    #[must_use = "wake the driver"]
    pub fn flush(&mut self, qconn: &mut QuicheConnection) -> quiche::Result<Option<Waker>> {
        if self.reset.is_some() {
            return Ok(self.blocked.take());
        }

        if let Some(code) = self.stop {
            tracing::trace!(stream_id = ?self.id, code, "sending STOP_SENDING");
            qconn.stream_shutdown(self.id.into(), quiche::Shutdown::Read, code)?;
            self.closed = true;
            return Ok(self.blocked.take());
        }

        let mut changed = false;

        while self.max > 0 {
            if self.buf.capacity() == 0 {
                // TODO get the readable size in Quiche so we can use that instead of guessing.
                self.buf_capacity = (self.buf_capacity * 2).min(32 * 1024);
                self.buf.reserve(self.buf_capacity);
            }

            // We don't actually use the buffer.len() because we immediately call split_to after reading.
            assert!(
                self.buf.is_empty(),
                "buffer should always be empty (but have capacity)"
            );

            // Do some unsafe to avoid zeroing the buffer.
            let buf: &mut [u8] = unsafe {
                std::mem::transmute::<&mut [std::mem::MaybeUninit<u8>], &mut [u8]>(
                    self.buf.spare_capacity_mut(),
                )
            };
            let n = buf.len().min(self.max);

            match qconn.stream_recv(self.id.into(), &mut buf[..n]) {
                Ok((n, done)) => {
                    // Advance the buffer by the number of bytes read.
                    unsafe { self.buf.set_len(self.buf.len() + n) };

                    tracing::trace!(
                        stream_id = ?self.id,
                        size = n,
                        "received STREAM",
                    );

                    // Then split the buffer and push the front to the queue.
                    self.queued.push_back(self.buf.split_to(n).freeze());
                    self.max -= n;

                    changed = true;

                    if done {
                        tracing::trace!(stream_id = ?self.id, "received FIN");

                        self.fin = true;
                        self.closed = true;
                        return Ok(self.blocked.take());
                    }
                }
                Err(quiche::Error::Done) => {
                    if qconn.stream_finished(self.id.into()) {
                        tracing::trace!(stream_id = ?self.id, "received FIN");

                        self.fin = true;
                        self.closed = true;
                        return Ok(self.blocked.take());
                    }
                    break;
                }
                Err(quiche::Error::StreamReset(code)) => {
                    tracing::trace!(stream_id = ?self.id, code, "received RESET_STREAM");

                    self.reset = Some(code);
                    self.closed = true;
                    return Ok(self.blocked.take());
                }
                Err(e) => return Err(e),
            }
        }

        if changed {
            Ok(self.blocked.take())
        } else {
            // Don't wake up the application if nothing was received.
            Ok(None)
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

/// A stream that can be used to receive bytes.
pub struct RecvStream {
    id: StreamId,
    state: Lock<RecvState>,
    driver: Lock<DriverState>,
}

impl RecvStream {
    pub(super) fn new(id: StreamId, state: Lock<RecvState>, driver: Lock<DriverState>) -> Self {
        Self { id, state, driver }
    }

    /// Returns the QUIC stream ID.
    pub fn id(&self) -> StreamId {
        self.id
    }

    /// Read some data into the buffer and return the amount read.
    ///
    /// Returns [None] if the stream has been finished by the remote.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, StreamError> {
        Ok(self.read_chunk(buf.len()).await?.map(|chunk| {
            buf[..chunk.len()].copy_from_slice(&chunk);
            chunk.len()
        }))
    }

    /// Read a chunk of data from the stream, avoiding a copy.
    ///
    /// Returns [None] if the stream has been finished by the remote.
    pub async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, StreamError> {
        poll_fn(|cx| self.poll_read_chunk(cx.waker(), max)).await
    }

    fn poll_read_chunk(
        &mut self,
        waker: &Waker,
        max: usize,
    ) -> Poll<Result<Option<Bytes>, StreamError>> {
        if let Poll::Ready(res) = self.state.lock().poll_read_chunk(waker, max) {
            return Poll::Ready(res);
        }

        let mut driver = self.driver.lock();

        // Check if the connection is closed.
        if let Poll::Ready(res) = driver.closed(waker) {
            return Poll::Ready(Err(res.into()));
        }

        // If we're blocked, tell the driver we want more data.
        let waker = driver.recv(self.id);
        if let Some(waker) = waker {
            waker.wake();
        }

        Poll::Pending
    }

    /// Read data into a mutable buffer and return the amount read.
    ///
    /// The buffer will be advanced by the number of bytes read.
    /// Returns [None] if the stream has been finished by the remote.
    pub async fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Result<Option<usize>, StreamError> {
        match self
            .read(unsafe {
                std::mem::transmute::<&mut bytes::buf::UninitSlice, &mut [u8]>(buf.chunk_mut())
            })
            .await?
        {
            Some(n) if n > 0 => {
                unsafe { buf.advance_mut(n) };
                Ok(Some(n))
            }
            _ => Ok(None),
        }
    }

    /// Read until the end of the stream (or the limit is hit).
    pub async fn read_all(&mut self, max: usize) -> Result<Bytes, StreamError> {
        let buf = BytesMut::new();
        let mut limit = buf.limit(max);
        while limit.has_remaining_mut() && self.read_buf(&mut limit).await?.is_some() {}
        Ok(limit.into_inner().freeze())
    }

    /// Tell the other end to stop sending data with the given error code.
    ///
    /// This sends a STOP_SENDING frame to the remote.
    pub fn stop(&mut self, code: u64) {
        self.state.lock().stop = Some(code);

        let waker = self.driver.lock().recv(self.id);
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Returns true if the stream is closed by either side.
    ///
    /// This includes:
    /// - We sent a STOP_SENDING via [RecvStream::stop]
    /// - We received a RESET_STREAM from the remote
    /// - We received a FIN from the remote
    pub fn is_closed(&self) -> bool {
        self.state.lock().is_closed()
    }

    fn poll_closed(&mut self, waker: &Waker) -> Poll<Result<(), StreamError>> {
        if let Poll::Ready(res) = self.state.lock().poll_closed(waker) {
            return Poll::Ready(res);
        }

        if let Poll::Ready(res) = self.driver.lock().closed(waker) {
            return Poll::Ready(Err(res.into()));
        }

        Poll::Pending
    }

    /// Wait until the stream is closed by either side.
    ///
    /// This includes:
    /// - We sent a STOP_SENDING via [RecvStream::stop]
    /// - We received a RESET_STREAM from the remote
    /// - We received a FIN from the remote
    ///
    /// **NOTE**: This takes `&mut` to match quiche and slightly simplify the implementation.
    pub async fn closed(&mut self) -> Result<(), StreamError> {
        poll_fn(|cx| self.poll_closed(cx.waker())).await
    }
}

impl Drop for RecvStream {
    fn drop(&mut self) {
        let mut state = self.state.lock();

        if !state.fin && state.reset.is_none() && state.stop.is_none() {
            state.stop = Some(DROP_CODE);
            // Avoid two locks at once.
            drop(state);

            let waker = self.driver.lock().recv(self.id);
            if let Some(waker) = waker {
                waker.wake();
            }
        }
    }
}

impl AsyncRead for RecvStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), io::Error>> {
        match ready!(self.poll_read_chunk(cx.waker(), buf.remaining())) {
            Ok(Some(chunk)) => buf.put_slice(&chunk),
            Ok(None) => {}
            Err(e) => return Poll::Ready(Err(io::Error::other(e.to_string()))),
        };
        Poll::Ready(Ok(()))
    }
}
