use kio::Waiter;
use std::{
    collections::VecDeque,
    io,
    pin::Pin,
    task::{ready, Context, Poll, Waker},
};
use tokio_quiche::quiche::{self};

use bytes::{Buf, Bytes};
use tokio::io::AsyncWrite;

use tokio_quiche::quic::QuicheConnection;

use crate::{ez::DriverState, waiters::Parked};

use super::{Lock, StreamError, StreamId};

// "send" in ascii; if you see this then call finish().await or close(code)
const DROP_CODE: u64 = 0x73656E64;

/// The priority every stream starts at.
const DEFAULT_PRIORITY: u8 = 0;

/// quiche schedules lower values first, so flip our higher-is-first priority to match.
const fn quiche_urgency(priority: u8) -> u8 {
    u8::MAX - priority
}

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

    // quiche discarded the stream before we asked it anything, so the peer stopped us
    // but the code is gone. Terminal exactly like `stop`, with nothing to report.
    gone: bool,

    // pending SET_PRIORITY, higher is sent first
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
            gone: false,
            // Pin every stream to the default rather than inheriting quiche's, so that a
            // stream explicitly assigned a low priority can't be outranked by an untouched one.
            priority: Some(DEFAULT_PRIORITY),
            closed: false,
        }
    }

    // Write some of the buffer to the stream, advancing the internal position.
    // Returns the number of bytes written for convenience.
    fn poll_write_buf<B: Buf>(
        &mut self,
        waiter: &Waiter,
        buf: &mut B,
    ) -> Poll<Result<usize, StreamError>> {
        if let Some(reset) = self.reset {
            return Poll::Ready(Err(StreamError::Reset(reset)));
        } else if let Some(stop) = self.stop {
            return Poll::Ready(Err(StreamError::Stop(stop)));
        } else if self.gone || self.fin {
            return Poll::Ready(Err(StreamError::Closed));
        }

        if self.capacity == 0 {
            // A stream has a single writer, so one slot is enough here — unlike the
            // connection-wide lists, which every stream and accepter parks on.
            self.blocked = Some(waiter.waker().clone());
            return Poll::Pending;
        }

        let n = self.capacity.min(buf.remaining());

        // NOTE: Avoids a copy when Buf is Bytes.
        let chunk = buf.copy_to_bytes(n);

        self.capacity -= chunk.len();
        self.queued.push_back(chunk);

        Poll::Ready(Ok(n))
    }

    pub fn poll_closed(&mut self, waiter: &Waiter) -> Poll<Result<(), StreamError>> {
        if let Some(reset) = self.reset {
            return Poll::Ready(Err(StreamError::Reset(reset)));
        } else if let Some(stop) = self.stop {
            return Poll::Ready(Err(StreamError::Stop(stop)));
        } else if self.gone {
            return Poll::Ready(Err(StreamError::Closed));
        } else if self.closed {
            // self.closed means we sent the FIN already
            // TODO wait until the peer has acknowledged the fin
            return Poll::Ready(Ok(()));
        }

        self.blocked = Some(waiter.waker().clone());

        Poll::Pending
    }

    pub fn poll_flushed(&mut self, waiter: &Waiter) -> Poll<Result<(), StreamError>> {
        if let Some(reset) = self.reset {
            return Poll::Ready(Err(StreamError::Reset(reset)));
        } else if let Some(stop) = self.stop {
            return Poll::Ready(Err(StreamError::Stop(stop)));
        } else if self.gone {
            // The queue was discarded rather than sent, so reporting a successful
            // flush here would tell the application its bytes reached the peer.
            return Poll::Ready(Err(StreamError::Closed));
        } else if self.queued.is_empty() {
            return Poll::Ready(Ok(()));
        }

        self.blocked = Some(waiter.waker().clone());

        Poll::Pending
    }

    /// Close this stream in response to a quiche error, or propagate a genuine
    /// connection error.
    ///
    /// quiche reports the end of an individual stream through errors on otherwise
    /// ordinary calls: `StreamStopped` once the peer has sent STOP_SENDING, and
    /// `Done` or `InvalidStreamState` once quiche has reset the stream and collected
    /// its state. All three mean this stream is over; none of them mean the
    /// connection is, and promoting one would tear down every other stream on the
    /// session.
    #[must_use = "wake the driver"]
    fn closed_by(&mut self, err: quiche::Error) -> quiche::Result<Option<Waker>> {
        match err {
            quiche::Error::StreamStopped(code) => {
                tracing::trace!(stream_id = ?self.id, code, "received STOP_SENDING");
                self.stop = Some(code);
            }
            quiche::Error::Done | quiche::Error::InvalidStreamState(_) => {
                tracing::trace!(stream_id = ?self.id, "stream already collected by quiche");
                // Only a peer STOP_SENDING gets a live stream collected, so this is a
                // stop whose code quiche has already thrown away. It is not a `fin`:
                // the application's bytes never left, and saying otherwise would report
                // a successful flush for data the peer refused.
                self.gone = true;
            }
            e => return Err(e),
        }

        // The driver drops this state as soon as it sees `closed`, so nothing here will
        // ever be flushed again: leaving bytes queued would park a flush forever, and
        // leaving capacity behind would let writes keep accumulating into a queue that
        // no longer reaches the peer.
        self.queued.clear();
        self.capacity = 0;
        self.closed = true;

        Ok(self.blocked.take())
    }

    #[must_use = "wake the driver"]
    pub fn flush(&mut self, qconn: &mut QuicheConnection) -> quiche::Result<Option<Waker>> {
        if let Some(code) = self.reset {
            tracing::trace!(stream_id = ?self.id, code, "sending RESET_STREAM");
            if let Err(e) = qconn.stream_shutdown(self.id.into(), quiche::Shutdown::Write, code) {
                return self.closed_by(e);
            }
            self.closed = true;
            return Ok(self.blocked.take());
        }

        if self.stop.take().is_some() {
            return Ok(self.blocked.take());
        }

        if let Some(priority) = self.priority.take() {
            tracing::trace!(stream_id = ?self.id, priority, "updating STREAM");
            qconn.stream_priority(self.id.into(), quiche_urgency(priority), true)?;
        }

        while let Some(mut chunk) = self.queued.pop_front() {
            let n = match qconn.stream_send(self.id.into(), &chunk, false) {
                Ok(n) => n,
                // Out of connection-level capacity, so retry once writable again.
                // The same error also covers a collected stream, which the
                // `stream_writable` registration below reports as gone.
                Err(quiche::Error::Done) => 0,
                Err(e) => return self.closed_by(e),
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
                if let Err(e) = qconn.stream_writable(self.id.into(), 1) {
                    return self.closed_by(e);
                }

                break;
            }
        }

        if self.queued.is_empty() && self.fin {
            tracing::trace!(stream_id = ?self.id, "sending FIN");
            if let Err(e) = qconn.stream_send(self.id.into(), &[], true) {
                return self.closed_by(e);
            }

            self.closed = true;
            return Ok(self.blocked.take());
        }

        self.capacity = match qconn.stream_capacity(self.id.into()) {
            Ok(capacity) => capacity,
            Err(e) => return self.closed_by(e),
        };

        // A flush waiter can make progress as soon as the internal queue has
        // drained, even if there is no capacity available for another write.
        if self.queued.is_empty() || self.capacity > 0 {
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
        } else if self.gone {
            Err(StreamError::Closed)
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

    // For the `AsyncWrite` impl, which is handed a `Context` rather than a waiter.
    parked: Parked,
}

impl SendStream {
    pub(super) fn new(id: StreamId, state: Lock<SendState>, driver: Lock<DriverState>) -> Self {
        Self {
            id,
            state,
            driver,
            parked: Parked::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_test() -> Self {
        let id = StreamId::CLIENT_UNI;
        Self::new(
            id,
            Lock::new(SendState::new(id)),
            Lock::new(DriverState::new(false)),
        )
    }

    #[cfg(test)]
    pub(crate) fn priority(&self) -> Option<u8> {
        self.state.lock().priority
    }

    /// Returns the QUIC stream ID.
    pub fn id(&self) -> StreamId {
        self.id
    }

    /// Write some data to the stream, returning the size written.
    pub async fn write(&mut self, buf: &[u8]) -> Result<usize, StreamError> {
        let mut buf = io::Cursor::new(buf);
        kio::wait(|waiter| self.poll_write_buf(waiter, &mut buf)).await
    }

    /// Poll to write some data to the stream, returning the size written.
    pub fn poll_write(&mut self, waiter: &Waiter, buf: &[u8]) -> Poll<Result<usize, StreamError>> {
        let mut buf = io::Cursor::new(buf);
        self.poll_write_buf(waiter, &mut buf)
    }

    /// Poll to write some of the buffer to the stream, advancing the internal
    /// position.
    ///
    /// Returns the number of bytes written for convenience.
    pub fn poll_write_buf<B: Buf>(
        &mut self,
        waiter: &Waiter,
        buf: &mut B,
    ) -> Poll<Result<usize, StreamError>> {
        if let Poll::Ready(res) = self.state.lock().poll_write_buf(waiter, buf) {
            // Tell the driver that the stream has data to send.
            let waker = self.driver.lock().send(self.id);
            if let Some(waker) = waker {
                waker.wake();
            }

            return Poll::Ready(res);
        }

        if let Poll::Ready(res) = self.driver.lock().error(waiter) {
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
        kio::wait(|waiter| self.poll_write_buf(waiter, buf)).await
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
            } else if state.gone || state.fin {
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

    /// Poll until the stream is closed by either side.
    pub fn poll_closed(&mut self, waiter: &Waiter) -> Poll<Result<(), StreamError>> {
        if let Poll::Ready(res) = self.state.lock().poll_closed(waiter) {
            return Poll::Ready(res);
        }

        if let Poll::Ready(res) = self.driver.lock().error(waiter) {
            return Poll::Ready(Err(res.into()));
        }

        Poll::Pending
    }

    fn poll_flushed(&mut self, waiter: &Waiter) -> Poll<Result<(), StreamError>> {
        if let Poll::Ready(res) = self.state.lock().poll_flushed(waiter) {
            return Poll::Ready(res);
        }

        if let Poll::Ready(res) = self.driver.lock().closed(waiter) {
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
        kio::wait(|waiter| self.poll_closed(waiter)).await
    }

    /// Set the priority of this stream.
    ///
    /// Streams with a higher priority are sent first, but are not guaranteed to arrive first.
    /// Defaults to 0.
    ///
    /// Note that this is the opposite of quiche's urgency, which is inverted internally.
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

        if !state.fin && !state.gone && state.reset.is_none() && state.stop.is_none() {
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
        let waiter = self.parked.hold(cx);
        let res = self.poll_write_buf(&waiter, &mut buf);
        self.parked.settle(&res);

        match ready!(res) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(e) => Poll::Ready(Err(io::Error::other(e.to_string()))),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let waiter = self.parked.hold(cx);
        let res = self.poll_flushed(&waiter);
        self.parked.settle(&res);

        res.map_err(|e| io::Error::other(e.to_string()))
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        match self.is_finished() {
            Ok(false) => {
                if let Err(e) = self.finish() {
                    return Poll::Ready(Err(io::Error::other(e.to_string())));
                }
            }
            Ok(true) => {}
            Err(e) => return Poll::Ready(Err(io::Error::other(e.to_string()))),
        }

        let waiter = self.parked.hold(cx);
        let res = self.poll_closed(&waiter);
        self.parked.settle(&res);

        res.map_err(|e| io::Error::other(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The urgency handed to `quiche::Connection::stream_priority`.
    fn urgency(stream: &SendStream) -> u8 {
        quiche_urgency(stream.priority().expect("priority is set on construction"))
    }

    #[test]
    fn higher_priority_is_sent_first() {
        let mut low = SendStream::new_test();
        let mut high = SendStream::new_test();

        low.set_priority(1);
        high.set_priority(2);

        // quiche sends lower urgencies first.
        assert!(urgency(&high) < urgency(&low));
    }

    #[test]
    fn untouched_stream_is_outranked_by_any_promotion() {
        // quiche's own default urgency is 127, which would outrank anything below priority 128.
        for priority in [1, 55, 100, 128, 200, 255] {
            let untouched = SendStream::new_test();
            let mut promoted = SendStream::new_test();

            promoted.set_priority(priority);

            assert!(
                urgency(&promoted) < urgency(&untouched),
                "priority {priority} should outrank an untouched stream"
            );
        }
    }

    #[test]
    fn explicit_default_matches_untouched() {
        let untouched = SendStream::new_test();
        let mut explicit = SendStream::new_test();

        explicit.set_priority(DEFAULT_PRIORITY);

        assert_eq!(urgency(&explicit), urgency(&untouched));
    }
}
