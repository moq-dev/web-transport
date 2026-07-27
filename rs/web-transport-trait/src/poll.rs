//! A `poll`-based adapter over an async [`Session`].

use std::{
    ops::Deref,
    task::{Context, Poll},
};

use bytes::Bytes;

use crate::{OpState, Session};

/// The stream pair produced by opening or accepting a bidirectional stream.
pub type BiStreams<S> = (<S as Session>::SendStream, <S as Session>::RecvStream);

/// Drives a [`Session`]'s operations from a `poll`-based loop.
///
/// [`Session`] is async-native: every backend implements its session operations
/// as multi-step futures over a shared connection, so there is no cheaper poll
/// form for the trait itself to expose. This adapter provides the poll surface
/// once instead of asking each backend to reimplement it.
///
/// An operation that returns [`Poll::Pending`] is retained, so the next poll
/// resumes it rather than starting over. That matters most for `open_uni` and
/// `open_bi`, which claim stream credit before they resolve — restarting would
/// leak the claim.
///
/// Each adapter drives at most one operation of each kind. To run two of the
/// same kind concurrently, clone the session and build a second adapter.
///
/// [`Deref`] exposes the underlying session, including its async methods, which
/// are unaffected by anything in flight here.
///
/// ```no_run
/// # use std::task::{Context, Poll};
/// # use web_transport_trait::{Session, SessionPoll};
/// fn poll_streams<S: Session>(session: &mut SessionPoll<S>, cx: &mut Context<'_>) {
///     while let Poll::Ready(Ok(stream)) = session.poll_accept_uni(cx) {
///         drop(stream);
///     }
/// }
/// ```
pub struct SessionPoll<S: Session> {
    session: S,
    accept_uni: OpState<Result<S::RecvStream, S::Error>>,
    accept_bi: OpState<Result<BiStreams<S>, S::Error>>,
    open_uni: OpState<Result<S::SendStream, S::Error>>,
    open_bi: OpState<Result<BiStreams<S>, S::Error>>,
    recv_datagram: OpState<Result<Bytes, S::Error>>,
    closed: OpState<S::Error>,
}

impl<S: Session> SessionPoll<S> {
    /// Wrap a session so its operations can be driven by polling.
    pub fn new(session: S) -> Self {
        Self {
            session,
            accept_uni: Default::default(),
            accept_bi: Default::default(),
            open_uni: Default::default(),
            open_bi: Default::default(),
            recv_datagram: Default::default(),
            closed: Default::default(),
        }
    }

    /// Discard the poll state and return the underlying session.
    pub fn into_inner(self) -> S {
        self.session
    }

    /// Poll for a unidirectional stream created by the peer.
    pub fn poll_accept_uni(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<S::RecvStream, S::Error>> {
        let session = &self.session;
        self.accept_uni.poll(cx, || {
            let session = session.clone();
            async move { session.accept_uni().await }
        })
    }

    /// Poll for a bidirectional stream created by the peer.
    pub fn poll_accept_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<BiStreams<S>, S::Error>> {
        let session = &self.session;
        self.accept_bi.poll(cx, || {
            let session = session.clone();
            async move { session.accept_bi().await }
        })
    }

    /// Poll to open a unidirectional stream, which blocks while there are too
    /// many concurrent streams.
    pub fn poll_open_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<S::SendStream, S::Error>> {
        let session = &self.session;
        self.open_uni.poll(cx, || {
            let session = session.clone();
            async move { session.open_uni().await }
        })
    }

    /// Poll to open a bidirectional stream, which blocks while there are too
    /// many concurrent streams.
    pub fn poll_open_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<BiStreams<S>, S::Error>> {
        let session = &self.session;
        self.open_bi.poll(cx, || {
            let session = session.clone();
            async move { session.open_bi().await }
        })
    }

    /// Poll for a datagram from the network.
    pub fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, S::Error>> {
        let session = &self.session;
        self.recv_datagram.poll(cx, || {
            let session = session.clone();
            async move { session.recv_datagram().await }
        })
    }

    /// Poll until the connection is closed by either side.
    pub fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<S::Error> {
        let session = &self.session;
        self.closed.poll(cx, || {
            let session = session.clone();
            async move { session.closed().await }
        })
    }
}

impl<S: Session> Deref for SessionPoll<S> {
    type Target = S;

    fn deref(&self) -> &S {
        &self.session
    }
}

impl<S: Session> From<S> for SessionPoll<S> {
    fn from(session: S) -> Self {
        Self::new(session)
    }
}

/// Cloning yields an independent set of idle operations, so the clone can drive
/// its own `accept`/`open` concurrently with this one.
impl<S: Session> Clone for SessionPoll<S> {
    fn clone(&self) -> Self {
        Self::new(self.session.clone())
    }
}

impl<S: Session + std::fmt::Debug> std::fmt::Debug for SessionPoll<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionPoll")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}
