use std::{
    collections::VecDeque,
    future::poll_fn,
    io,
    pin::Pin,
    task::{ready, Context, Poll, Waker},
};
use tokio_quiche::quiche::{self};

use bytes::{Buf, Bytes};
use tokio::io::AsyncWrite;

use tokio_quiche::quic::QuicheConnection;

use crate::ez::DriverState;

use super::{Lock, StreamError, StreamId};

// "send" in ascii; if you see this then call finish().await or close(code)
const DROP_CODE: u64 = 0x73656E64;

// TODO Move a lot of this into a state machine enum.
pub(super) struct SendState {
    id: StreamId,

    // The amount of data that is allowed to be written.
    capacity: usize,

    // Data ready to send. (capacity has been subtracted)
    queued: VecDeque<Bytes>,

    // Called by the driver when the stream is writable again.
    blocked: Option<Waker>,

    // send STREAM_FIN
    fin: bool,

    // send RESET_STREAM
    reset: Option<u64>,

    // received
    stop: Option<u64>,

    // received SET_PRIORITY
    priority: Option<u8>,

    // No more progress can be made on the stream.
    closed: bool,
}

impl SendState {
    pub fn new(id: StreamId) -> Self {
        Self {
            id,
            capacity: 0,
            queued: VecDeque::new(),
            blocked: None,
            fin: false,
            reset: None,
            stop: None,
            priority: None,
            closed: false,
        }
    }

    // Write some of the buffer to the stream, advancing the internal position.
    // Returns the number of bytes written for convenience.
    fn poll_write_buf<B: Buf>(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<Result<usize, StreamError>> {
        if let Some(reset) = self.reset {
            return Poll::Ready(Err(StreamError::Reset(reset)));
        } else if let Some(stop) = self.stop {
            return Poll::Ready(Err(StreamError::Stop(stop)));
        } else if self.fin {
            return Poll::Ready(Err(StreamError::Closed));
        }

        if self.capacity == 0 {
            self.blocked = Some(cx.waker().clone());
            return Poll::Pending;
        }

        let n = self.capacity.min(buf.remaining());

        // NOTE: Avoids a copy when Buf is Bytes.
        let chunk = buf.copy_to_bytes(n);

        self.capacity -= chunk.len();
        self.queued.push_back(chunk);

        Poll::Ready(Ok(n))
    }

    pub fn poll_closed(&mut self, waker: &Waker) -> Poll<Result<(), StreamError>> {
        if let Some(reset) = self.reset {
            return Poll::Ready(Err(StreamError::Reset(reset)));
        } else if let Some(stop) = self.stop {
            return Poll::Ready(Err(StreamError::Stop(stop)));
        } else if self.closed {
            // self.closed means we sent the FIN already
            // TODO wait until the peer has acknowledged the fin
            return Poll::Ready(Ok(()));
        }

        self.blocked = Some(waker.clone());

        Poll::Pending
    }

    #[must_use = "wake the driver"]
    pub fn flush(&mut self, qconn: &mut QuicheConnection) -> quiche::Result<Option<Waker>> {
        if let Some(code) = self.reset {
            tracing::trace!(stream_id = ?self.id, code, "sending RESET_STREAM");
            qconn.stream_shutdown(self.id.into(), quiche::Shutdown::Write, code)?;
            self.closed = true;
            return Ok(self.blocked.take());
        }

        if self.stop.take().is_some() {
            return Ok(self.blocked.take());
        }

        if let Some(priority) = self.priority.take() {
            tracing::trace!(stream_id = ?self.id, priority, "updating STREAM");
            qconn.stream_priority(self.id.into(), priority, true)?;
        }

        while let Some(mut chunk) = self.queued.pop_front() {
            let n = match qconn.stream_send(self.id.into(), &chunk, false) {
                Ok(n) => n,
                Err(quiche::Error::Done) => 0,
                Err(quiche::Error::StreamStopped(code)) => {
                    tracing::trace!(stream_id = ?self.id, code, "received STOP_SENDING");

                    self.stop = Some(code);
                    self.closed = true;
                    return Ok(self.blocked.take());
                }
                Err(e) => return Err(e),
            };

            tracing::trace!(
                stream_id = ?self.id,
                size = n,
                "sent STREAM",
            );

            if n < chunk.len() {
                // NOTE: This logic should rarely be executed because we gate based on stream capacity.

                let remaining = chunk.split_off(n);
                self.queued.push_front(remaining);

                // Register a `stream_writable_next` callback when at least one byte is ready to send.
                qconn.stream_writable(self.id.into(), 1)?;

                break;
            }
        }

        if self.queued.is_empty() && self.fin {
            tracing::trace!(stream_id = ?self.id, "sending FIN");
            qconn.stream_send(self.id.into(), &[], true)?;

            self.closed = true;
            return Ok(self.blocked.take());
        }

        self.capacity = match qconn.stream_capacity(self.id.into()) {
            Ok(capacity) => capacity,
            Err(quiche::Error::StreamStopped(code)) => {
                tracing::trace!(stream_id = ?self.id, code, "received STOP_SENDING");

                self.stop = Some(code);
                self.closed = true;
                return Ok(self.blocked.take());
            }
            Err(e) => return Err(e),
        };

        if self.capacity > 0 {
            return Ok(self.blocked.take());
        }

        // No write capacity available, so don't wake up the application.
        Ok(None)
    }

    pub fn is_finished(&self) -> Result<bool, StreamError> {
        if let Some(reset) = self.reset {
            Err(StreamError::Reset(reset))
        } else if let Some(stop) = self.stop {
            Err(StreamError::Stop(stop))
        } else {
            Ok(self.fin)
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

/// A stream that can be used to send bytes.
pub struct SendStream {
    id: StreamId,
    state: Lock<SendState>,
    driver: Lock<DriverState>,
}

impl SendStream {
    pub(super) fn new(id: StreamId, state: Lock<SendState>, driver: Lock<DriverState>) -> Self {
        Self { id, state, driver }
    }

    /// Returns the QUIC stream ID.
    pub fn id(&self) -> StreamId {
        self.id
    }

    /// Write some data to the stream, returning the size written.
    pub async fn write(&mut self, buf: &[u8]) -> Result<usize, StreamError> {
        let mut buf = io::Cursor::new(buf);
        poll_fn(|cx| self.poll_write_buf(cx, &mut buf)).await
    }

    // Write some of the buffer to the stream, advancing the internal position.
    //
    // Returns the number of bytes written for convenience.
    fn poll_write_buf<B: Buf>(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<Result<usize, StreamError>> {
        if let Poll::Ready(res) = self.state.lock().poll_write_buf(cx, buf) {
            // Tell the driver that the stream has data to send.
            let waker = self.driver.lock().send(self.id);
            if let Some(waker) = waker {
                waker.wake();
            }

            return Poll::Ready(res);
        }

        if let Poll::Ready(res) = self.driver.lock().closed(cx.waker()) {
            return Poll::Ready(Err(res.into()));
        }

        Poll::Pending
    }

    /// Write all of the slice to the stream.
    pub async fn write_all(&mut self, mut buf: &[u8]) -> Result<(), StreamError> {
        while !buf.is_empty() {
            let n = self.write(buf).await?;
            buf = &buf[n..];
        }
        Ok(())
    }

    /// Write some of the buffer to the stream, advancing the internal position.
    ///
    /// Returns the number of bytes written for convenience.
    pub async fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Result<usize, StreamError> {
        poll_fn(|cx| self.poll_write_buf(cx, buf)).await
    }

    /// Write the entire buffer to the stream, advancing the internal position.
    pub async fn write_buf_all<B: Buf>(&mut self, buf: &mut B) -> Result<(), StreamError> {
        while buf.has_remaining() {
            self.write_buf(buf).await?;
        }
        Ok(())
    }

    /// Mark the stream as finished, such that no more data can be written.
    ///
    /// [SendStream::closed] will block until the FIN has been sent.
    ///
    /// **WARN**: If this is not called explicitly, [SendStream::reset] will be called on [Drop].
    pub fn finish(&mut self) -> Result<(), StreamError> {
        {
            let mut state = self.state.lock();
            if let Some(reset) = state.reset {
                return Err(StreamError::Reset(reset));
            } else if let Some(stop) = state.stop {
                return Err(StreamError::Stop(stop));
            } else if state.fin {
                return Err(StreamError::Closed);
            }

            state.fin = true;
        }

        let waker = self.driver.lock().send(self.id);
        if let Some(waker) = waker {
            waker.wake();
        }

        Ok(())
    }

    /// Returns true if [SendStream::finish] has been called, or if the stream has been closed by the peer.
    pub fn is_finished(&self) -> Result<bool, StreamError> {
        self.state.lock().is_finished()
    }

    /// Abruptly reset the stream with the provided error code.
    ///
    /// This sends a RESET_STREAM frame to the remote.
    pub fn reset(&mut self, code: u64) {
        self.state.lock().reset = Some(code);

        let waker = self.driver.lock().send(self.id);
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Returns true if the stream is closed by either side.
    ///
    /// This includes:
    /// - We sent a RESET_STREAM via [SendStream::reset]
    /// - We received a STOP_SENDING via [super::RecvStream::stop]
    /// - We sent a FIN via [SendStream::finish]
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
    /// - We sent a RESET_STREAM via [SendStream::reset]
    /// - We received a STOP_SENDING via [super::RecvStream::stop]
    /// - We sent a FIN via [SendStream::finish]
    ///
    /// Note: This takes `&mut` to match quiche and to simplify the implementation.
    pub async fn closed(&mut self) -> Result<(), StreamError> {
        poll_fn(|cx| self.poll_closed(cx.waker())).await
    }

    /// Set the priority of this stream.
    ///
    /// Lower priority values are sent first. Defaults to 0.
    pub fn set_priority(&mut self, priority: u8) {
        self.state.lock().priority = Some(priority);

        let waker = self.driver.lock().send(self.id);
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl Drop for SendStream {
    fn drop(&mut self) {
        let mut state = self.state.lock();

        if !state.fin && state.reset.is_none() && state.stop.is_none() {
            // Reset the stream if we're dropped without calling finish.
            state.reset = Some(DROP_CODE);
            drop(state);

            let waker = self.driver.lock().send(self.id);
            if let Some(waker) = waker {
                waker.wake();
            }
        }
    }
}

impl AsyncWrite for SendStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let mut buf = io::Cursor::new(buf);
        match ready!(self.poll_write_buf(cx, &mut buf)) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(e) => Poll::Ready(Err(io::Error::other(e.to_string()))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        // Flushing happens automatically via the driver
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        match self.finish() {
            Ok(()) => match self.poll_closed(cx.waker()) {
                Poll::Ready(res) => Poll::Ready(res.map_err(|e| io::Error::other(e.to_string()))),
                Poll::Pending => Poll::Pending,
            },
            Err(e) => Poll::Ready(Err(io::Error::other(e.to_string()))),
        }
    }
}
