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

/// The send order every stream starts at.
pub const DEFAULT_PRIORITY: i32 = 0;

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
        Self::new_test_on(&Lock::new(DriverState::new(false)), StreamId::CLIENT_UNI)
    }

    /// A stream registered on an existing driver, so several can be ranked against
    /// each other the way one connection's streams are. Ranking is relative, so a
    /// stream with a driver to itself has nothing to be ordered against.
    #[cfg(test)]
    pub(super) fn new_test_on(driver: &Lock<DriverState>, id: StreamId) -> Self {
        driver.lock().register_send(id);
        Self::new(id, Lock::new(SendState::new(id)), driver.clone())
    }

    #[cfg(test)]
    pub(crate) fn priority(&self) -> Option<i32> {
        self.driver.lock().priority_of(self.id)
    }

    #[cfg(test)]
    pub(crate) fn urgency(&self) -> Option<u8> {
        self.driver.lock().urgency_of(self.id)
    }

    /// Returns the QUIC stream ID.
    pub fn id(&self) -> StreamId {
        self.id
    }

    /// Tell the driver this stream has work to flush.
    ///
    /// Skipped once the state is closed: the driver retires a stream as soon as it
    /// observes that, so a notification afterwards names a stream it no longer
    /// tracks and is reported as a spurious wakeup.
    fn notify(&self) {
        // Take the two locks in sequence, never both at once.
        let closed = self.state.lock().is_closed();
        if closed {
            return;
        }

        if let Some(waker) = self.driver.lock().send(self.id) {
            waker.wake();
        }
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
        // Bind before notifying: on edition 2021 the guard from an `if let` scrutinee
        // lives for the whole block, and `notify` takes the same lock.
        let polled = self.state.lock().poll_write_buf(waiter, buf);
        if let Poll::Ready(res) = polled {
            // Tell the driver that the stream has data to send.
            self.notify();

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

        self.notify();

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

        self.notify();
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
    /// Streams with a **higher** value are sent first, but are not guaranteed to
    /// arrive first. Defaults to [`DEFAULT_PRIORITY`]. This matches the W3C
    /// WebTransport `sendOrder` convention and the other `web-transport` backends.
    ///
    /// quiche schedules by an 8-bit urgency, so the `i32` is a *relative* order
    /// rather than a value quiche sees: the connection ranks its send streams and
    /// gives the 256 highest-priority levels an urgency each. Streams past that
    /// share the last one, as do streams with equal priority (which round-robin).
    pub fn set_priority(&mut self, order: i32) {
        let waker = self.driver.lock().set_priority(self.id, order);
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

            self.notify();
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
        stream.urgency().expect("stream is ranked on construction")
    }

    /// Streams have to share a driver to be ranked against each other, since the
    /// ranking is connection-wide.
    fn connection<const N: usize>(orders: [i32; N]) -> [SendStream; N] {
        let driver = Lock::new(DriverState::new(false));
        std::array::from_fn(|i| {
            // Distinct client-uni ids: 2, 6, 10, ...
            let mut stream = SendStream::new_test_on(&driver, StreamId::from(2 + i as u64 * 4));
            stream.set_priority(orders[i]);
            stream
        })
    }

    #[test]
    fn higher_priority_is_sent_first() {
        let [low, high] = connection([1, 2]);

        // quiche sends lower urgencies first.
        assert!(urgency(&high) < urgency(&low));
    }

    #[test]
    fn untouched_stream_is_outranked_by_any_promotion() {
        // quiche's own default urgency is 127, which would outrank anything below
        // priority 128 if the stream were left to inherit it.
        for priority in [1, 55, 100, 128, 200, 255, i32::MAX] {
            let driver = Lock::new(DriverState::new(false));
            let untouched = SendStream::new_test_on(&driver, StreamId::CLIENT_UNI);
            let mut promoted = SendStream::new_test_on(&driver, StreamId::from(6));

            promoted.set_priority(priority);

            assert!(
                urgency(&promoted) < urgency(&untouched),
                "priority {priority} should outrank an untouched stream"
            );
        }
    }

    /// The mirror of the above, and the reason the `i32` is signed: a stream can ask
    /// to go *behind* everything that never set a priority.
    #[test]
    fn a_demoted_stream_falls_behind_an_untouched_one() {
        for priority in [-1, -55, -1000, i32::MIN] {
            let driver = Lock::new(DriverState::new(false));
            let untouched = SendStream::new_test_on(&driver, StreamId::CLIENT_UNI);
            let mut demoted = SendStream::new_test_on(&driver, StreamId::from(6));

            demoted.set_priority(priority);

            assert!(
                urgency(&demoted) > urgency(&untouched),
                "priority {priority} should fall behind an untouched stream"
            );
        }
    }

    #[test]
    fn explicit_default_matches_untouched() {
        let driver = Lock::new(DriverState::new(false));
        let untouched = SendStream::new_test_on(&driver, StreamId::CLIENT_UNI);
        let mut explicit = SendStream::new_test_on(&driver, StreamId::from(6));

        explicit.set_priority(DEFAULT_PRIORITY);

        assert_eq!(urgency(&explicit), urgency(&untouched));
    }

    /// Equal priorities share a band so quiche round-robins them, rather than
    /// inventing a strict order between streams the caller called equal.
    #[test]
    fn equal_priorities_share_an_urgency() {
        let [a, b] = connection([7, 7]);

        assert_eq!(urgency(&a), urgency(&b));
    }
}
