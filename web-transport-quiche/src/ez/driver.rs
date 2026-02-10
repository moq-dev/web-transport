use std::{
    collections::{hash_map, HashMap, HashSet},
    future::poll_fn,
    task::{Poll, Waker},
};
use tokio_quiche::{
    buf_factory::{BufFactory, PooledBuf},
    quic::{HandshakeInfo, QuicheConnection},
};

use crate::ez::Lock;

use super::{
    ConnectionClosed, ConnectionError, Metrics, RecvState, RecvStream, SendState, SendStream,
    StreamId,
};

// "conndrop" in ascii; if you see this then close(code)
// decimal: 8029476563109179248
const DROP_CODE: u64 = 0x6F6E6E6464726F70;

type OpenBiResult =
    Poll<Result<(Option<Waker>, StreamId, Lock<SendState>, Lock<RecvState>), ConnectionError>>;
type OpenUniResult = Poll<Result<(Option<Waker>, StreamId, Lock<SendState>), ConnectionError>>;

pub(super) struct DriverState {
    send: HashSet<StreamId>,
    recv: HashSet<StreamId>,
    waker: Option<Waker>,

    bi: DriverOpen<(Lock<SendState>, Lock<RecvState>)>,
    uni: DriverOpen<Lock<SendState>>,

    local: ConnectionClosed,
    remote: ConnectionClosed,

    /// The negotiated ALPN protocol, set after the handshake completes.
    alpn: Option<Vec<u8>>,

    /// Wakers waiting for the handshake to complete.
    handshake_wakers: Vec<Waker>,
}

impl DriverState {
    pub fn new(server: bool) -> Self {
        let next_uni = match server {
            true => StreamId::SERVER_UNI,
            false => StreamId::CLIENT_UNI,
        };
        let next_bi = match server {
            true => StreamId::SERVER_BI,
            false => StreamId::CLIENT_BI,
        };

        Self {
            send: HashSet::new(),
            recv: HashSet::new(),
            waker: None,
            local: ConnectionClosed::default(),
            remote: ConnectionClosed::default(),
            bi: DriverOpen::new(next_bi),
            uni: DriverOpen::new(next_uni),
            alpn: None,
            handshake_wakers: Vec::new(),
        }
    }

    pub fn close(&mut self, err: ConnectionError) -> Vec<Waker> {
        self.local.abort(err)
    }

    pub fn closed(&self, waker: &Waker) -> Poll<ConnectionError> {
        self.local.poll(waker)
    }

    pub fn is_closed(&self) -> bool {
        self.local.is_closed() || self.remote.is_closed()
    }

    /// Returns the negotiated ALPN protocol, if the handshake has completed.
    pub fn alpn(&self) -> Option<&[u8]> {
        self.alpn.as_deref()
    }

    /// Poll for handshake completion.
    /// Returns Ready once the handshake completes, or if the connection is closed.
    pub fn poll_handshake(&mut self, waker: &Waker) -> Poll<Result<(), ConnectionError>> {
        // Check if already established
        if self.alpn.is_some() {
            return Poll::Ready(Ok(()));
        }

        // Check if connection is closed
        if let Poll::Ready(err) = self.local.poll(waker) {
            return Poll::Ready(Err(err));
        }
        if let Poll::Ready(err) = self.remote.poll(waker) {
            return Poll::Ready(Err(err));
        }

        // Wait for handshake
        self.handshake_wakers.push(waker.clone());
        Poll::Pending
    }

    /// Notify all wakers waiting for handshake completion.
    /// Should be called when the handshake completes.
    #[must_use = "wake the handshake wakers"]
    pub fn complete_handshake(&mut self) -> Vec<Waker> {
        std::mem::take(&mut self.handshake_wakers)
    }

    #[must_use = "wake the driver"]
    pub fn send(&mut self, stream_id: StreamId) -> Option<Waker> {
        if !self.send.insert(stream_id) {
            return None;
        }

        // You should call wake() without holding the lock.
        self.waker.take()
    }

    #[must_use = "wake the driver"]
    pub fn recv(&mut self, stream_id: StreamId) -> Option<Waker> {
        if !self.recv.insert(stream_id) {
            return None;
        }

        // You should call wake() without holding the lock.
        self.waker.take()
    }

    // Try to create the next bidirectional stream, although it may not be possible yet.
    pub fn open_bi(&mut self, waker: &Waker) -> OpenBiResult {
        if let Poll::Ready(err) = self.local.poll(waker) {
            return Poll::Ready(Err(err));
        }

        if self.bi.capacity == 0 {
            self.bi.wakers.push(waker.clone());
            return Poll::Pending;
        }
        self.bi.capacity -= 1;

        let id = self.bi.next.increment();
        tracing::trace!(?id, "opening bidirectional stream");

        let send = Lock::new(SendState::new(id));
        let recv = Lock::new(RecvState::new(id));
        self.bi.create.push((id, (send.clone(), recv.clone())));

        let wakeup = self.waker.take();
        Poll::Ready(Ok((wakeup, id, send, recv)))
    }

    pub fn open_uni(&mut self, waker: &Waker) -> OpenUniResult {
        if let Poll::Ready(err) = self.local.poll(waker) {
            return Poll::Ready(Err(err));
        }

        if self.uni.capacity == 0 {
            self.uni.wakers.push(waker.clone());
            return Poll::Pending;
        }

        self.uni.capacity -= 1;

        let id = self.uni.next.increment();
        tracing::trace!(?id, "opening unidirectional stream");

        let send = Lock::new(SendState::new(id));
        self.uni.create.push((id, send.clone()));

        let wakeup = self.waker.take();
        Poll::Ready(Ok((wakeup, id, send)))
    }
}

pub(super) struct Driver {
    state: Lock<DriverState>,

    send: HashMap<StreamId, Lock<SendState>>,
    recv: HashMap<StreamId, Lock<RecvState>>,

    buf: PooledBuf,

    accept_bi: flume::Sender<(SendStream, RecvStream)>,
    accept_uni: flume::Sender<RecvStream>,
}

impl Driver {
    pub fn new(
        state: Lock<DriverState>,
        accept_bi: flume::Sender<(SendStream, RecvStream)>,
        accept_uni: flume::Sender<RecvStream>,
    ) -> Self {
        Self {
            state,
            send: HashMap::new(),
            recv: HashMap::new(),
            buf: BufFactory::get_max_buf(),
            accept_bi,
            accept_uni,
        }
    }

    fn connected(
        &mut self,
        qconn: &mut QuicheConnection,
        _handshake_info: &HandshakeInfo,
    ) -> Result<(), ConnectionError> {
        // Capture the negotiated ALPN protocol.
        let alpn = qconn.application_proto();
        let wakers = {
            let mut state = self.state.lock();
            state.alpn = if alpn.is_empty() {
                None
            } else {
                Some(alpn.to_vec())
            };
            state.complete_handshake()
        };

        // Wake all tasks waiting for handshake completion.
        for waker in wakers {
            waker.wake();
        }

        // Run poll once to advance any pending operations.
        match self.poll(Waker::noop(), qconn) {
            Poll::Ready(Err(e)) => Err(e),
            _ => Ok(()),
        }
    }

    fn read(&mut self, qconn: &mut QuicheConnection) -> Result<(), ConnectionError> {
        while let Some(stream_id) = qconn.stream_readable_next() {
            let stream_id = StreamId::from(stream_id);

            tracing::trace!(?stream_id, "reading stream");

            if let hash_map::Entry::Occupied(mut entry) = self.recv.entry(stream_id) {
                let state = entry.get_mut();
                let mut state = state.lock();

                // Wake after dropping the lock to avoid deadlock
                let waker = state.flush(qconn)?;
                let closed = state.is_closed();
                drop(state);

                if closed {
                    entry.remove();
                }

                if let Some(waker) = waker {
                    waker.wake();
                }

                continue;
            }

            if stream_id.is_bi() {
                self.accept_bi(qconn, stream_id)?
            } else {
                self.accept_uni(qconn, stream_id)?
            }
        }

        Ok(())
    }

    fn accept_bi(
        &mut self,
        qconn: &mut QuicheConnection,
        stream_id: StreamId,
    ) -> Result<(), ConnectionError> {
        tracing::trace!(?stream_id, "accepting bidirectional stream");

        let mut state = RecvState::new(stream_id);
        state.flush(qconn)?;

        let state = Lock::new(state);

        self.recv.insert(stream_id, state.clone());
        let recv = RecvStream::new(stream_id, state.clone(), self.state.clone());

        let mut state = SendState::new(stream_id);
        state.flush(qconn)?;

        let state = Lock::new(state);
        self.send.insert(stream_id, state.clone());

        let send = SendStream::new(stream_id, state.clone(), self.state.clone());
        self.accept_bi
            .send((send, recv))
            .map_err(|_| ConnectionError::Dropped)?;

        Ok(())
    }

    fn accept_uni(
        &mut self,
        qconn: &mut QuicheConnection,
        stream_id: StreamId,
    ) -> Result<(), ConnectionError> {
        tracing::trace!(?stream_id, "accepting unidirectional stream");

        let mut state = RecvState::new(stream_id);
        state.flush(qconn)?;

        let state = Lock::new(state);
        self.recv.insert(stream_id, state.clone());

        let recv = RecvStream::new(stream_id, state.clone(), self.state.clone());
        self.accept_uni
            .send(recv)
            .map_err(|_| ConnectionError::Dropped)?;

        Ok(())
    }

    fn write(&mut self, qconn: &mut QuicheConnection) -> Result<(), ConnectionError> {
        while let Some(stream_id) = qconn.stream_writable_next() {
            let stream_id = StreamId::from(stream_id);

            match self.send.entry(stream_id) {
                hash_map::Entry::Occupied(mut entry) => {
                    let state = entry.get_mut();
                    let mut state = state.lock();

                    let waker = state.flush(qconn)?;
                    let closed = state.is_closed();
                    drop(state);

                    if closed {
                        entry.remove();
                    }

                    if let Some(waker) = waker {
                        waker.wake();
                    }
                }
                hash_map::Entry::Vacant(_entry) => {
                    tracing::warn!(?stream_id, "closed stream was writable");
                }
            }
        }

        Ok(())
    }

    async fn wait(&mut self, qconn: &mut QuicheConnection) -> Result<(), ConnectionError> {
        poll_fn(|cx| self.poll(cx.waker(), qconn)).await
    }

    fn poll(
        &mut self,
        waker: &Waker,
        qconn: &mut QuicheConnection,
    ) -> Poll<Result<(), ConnectionError>> {
        if !qconn.is_draining() {
            // Check if the application wants to close the connection.
            if let Poll::Ready(err) = self.state.lock().closed(waker) {
                // Close the connection and return the error.
                return Poll::Ready(
                    match err {
                        ConnectionError::Local(code, reason) => {
                            qconn.close(true, code, reason.as_bytes())
                        }
                        ConnectionError::Dropped => qconn.close(true, DROP_CODE, b"dropped"),
                        ConnectionError::Remote(code, reason) => {
                            // This shouldn't happen, but just echo it back in case.
                            qconn.close(true, code, reason.as_bytes())
                        }
                        ConnectionError::Quiche(e) => {
                            qconn.close(true, 500, e.to_string().as_bytes())
                        }
                        ConnectionError::Unknown(reason) => {
                            qconn.close(true, 501, reason.as_bytes())
                        }
                    }
                    .map_err(ConnectionError::Quiche),
                );
            }
        }

        // Don't try to do anything during the handshake.
        if !qconn.is_established() {
            return Poll::Pending;
        }

        let (sleep, send, recv, bi_wakers, uni_wakers) = {
            let mut driver = self.state.lock();
            driver.waker = Some(waker.clone());

            let sleep = driver.bi.create.is_empty()
                && driver.uni.create.is_empty()
                && driver.send.is_empty()
                && driver.recv.is_empty();

            for (id, (send, recv)) in driver.bi.create.drain(..) {
                qconn.stream_send(id.into(), &[], false)?;
                self.send.insert(id, send);
                self.recv.insert(id, recv);
            }

            for (id, send) in driver.uni.create.drain(..) {
                qconn.stream_send(id.into(), &[], false)?;
                self.send.insert(id, send);
            }

            // If we have spare capacity, wake up any blocked wakers.
            driver.bi.capacity = qconn.peer_streams_left_bidi();
            let bi_wakers = (driver.bi.capacity > 0).then(|| std::mem::take(&mut driver.bi.wakers));

            // If we have spare capacity, wake up any blocked wakers.
            driver.uni.capacity = qconn.peer_streams_left_uni();
            let uni_wakers =
                (driver.uni.capacity > 0).then(|| std::mem::take(&mut driver.uni.wakers));

            let send = std::mem::take(&mut driver.send);
            let recv = std::mem::take(&mut driver.recv);

            (sleep, send, recv, bi_wakers, uni_wakers)
        };

        for waker in bi_wakers.unwrap_or_default() {
            waker.wake();
        }

        for waker in uni_wakers.unwrap_or_default() {
            waker.wake();
        }

        for stream_id in recv {
            self.flush_recv(qconn, stream_id)?;
        }

        for stream_id in send {
            self.flush_send(qconn, stream_id)?;
        }

        if sleep {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn flush_recv(
        &mut self,
        qconn: &mut QuicheConnection,
        stream_id: StreamId,
    ) -> Result<(), ConnectionError> {
        if let hash_map::Entry::Occupied(mut entry) = self.recv.entry(stream_id) {
            let state = entry.get_mut();
            let mut state = state.lock();

            let waker = state.flush(qconn)?;
            let closed = state.is_closed();
            drop(state);

            if closed {
                entry.remove();
            }

            if let Some(waker) = waker {
                waker.wake();
            }
        } else {
            tracing::warn!(?stream_id, "wakeup for closed stream");
        }

        Ok(())
    }

    fn flush_send(
        &mut self,
        qconn: &mut QuicheConnection,
        stream_id: StreamId,
    ) -> Result<(), ConnectionError> {
        if let hash_map::Entry::Occupied(mut entry) = self.send.entry(stream_id) {
            let state = entry.get_mut();
            let mut state = state.lock();

            let waker = state.flush(qconn)?;
            let closed = state.is_closed();
            drop(state);

            if closed {
                entry.remove();
            }

            if let Some(waker) = waker {
                waker.wake();
            }
        } else {
            tracing::warn!(?stream_id, "wakeup for closed stream");
        }

        Ok(())
    }

    fn abort(&mut self, err: ConnectionError) {
        let wakers = self.state.lock().local.abort(err);
        for waker in wakers {
            waker.wake();
        }
    }
}

impl tokio_quiche::ApplicationOverQuic for Driver {
    fn on_conn_established(
        &mut self,
        qconn: &mut QuicheConnection,
        handshake_info: &tokio_quiche::quic::HandshakeInfo,
    ) -> tokio_quiche::QuicResult<()> {
        if let Err(e) = self.connected(qconn, handshake_info) {
            self.abort(e);
        }

        Ok(())
    }

    fn should_act(&self) -> bool {
        // TODO
        true
    }

    fn buffer(&mut self) -> &mut [u8] {
        &mut self.buf
    }

    async fn wait_for_data(
        &mut self,
        qconn: &mut QuicheConnection,
    ) -> Result<(), tokio_quiche::BoxError> {
        if let Err(e) = self.wait(qconn).await {
            self.abort(e.clone());
        }

        Ok(())
    }

    fn process_reads(&mut self, qconn: &mut QuicheConnection) -> tokio_quiche::QuicResult<()> {
        if let Err(e) = self.read(qconn) {
            self.abort(e);
        }

        Ok(())
    }

    fn process_writes(&mut self, qconn: &mut QuicheConnection) -> tokio_quiche::QuicResult<()> {
        if let Err(e) = self.write(qconn) {
            self.abort(e);
        }

        Ok(())
    }

    fn on_conn_close<M: Metrics>(
        &mut self,
        qconn: &mut QuicheConnection,
        _metrics: &M,
        connection_result: &tokio_quiche::QuicResult<()>,
    ) {
        let state = self.state.lock();

        let err = if let Poll::Ready(err) = state.local.poll(Waker::noop()) {
            err
        } else if let Some(local) = qconn.local_error() {
            let reason = String::from_utf8_lossy(&local.reason).to_string();
            ConnectionError::Local(local.error_code, reason)
        } else if let Some(peer) = qconn.peer_error() {
            let reason = String::from_utf8_lossy(&peer.reason).to_string();
            ConnectionError::Remote(peer.error_code, reason)
        } else if let Err(err) = connection_result {
            ConnectionError::Unknown(err.to_string())
        } else {
            ConnectionError::Unknown("no error message".to_string())
        };

        // Finally set the remote error once the connection is done.
        let wakers = state.remote.abort(err.clone());
        for waker in wakers {
            waker.wake();
        }

        // Also wake up any local wakers if the peer closed.
        let wakers = state.local.abort(err);
        for waker in wakers {
            waker.wake();
        }
    }
}

struct DriverOpen<T> {
    next: StreamId,
    capacity: u64,
    create: Vec<(StreamId, T)>,
    wakers: Vec<Waker>,
}

impl<T> DriverOpen<T> {
    pub fn new(next: StreamId) -> Self {
        Self {
            next,
            capacity: 0,
            create: Vec::new(),
            wakers: Vec::new(),
        }
    }
}
