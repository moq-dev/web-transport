//! Utility traits for conditional Send/Sync bounds, and a retained future for
//! exposing async operations through a `poll_*` API.
//!
//! The Send/Sync traits allow the same code to work on both native and WASM
//! targets, where WASM doesn't support Send/Sync.

use std::{
    future::Future,
    pin::Pin,
    sync::Mutex,
    task::{ready, Context, Poll},
};

#[cfg(not(target_family = "wasm"))]
type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

#[cfg(target_family = "wasm")]
type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + 'static>>;

/// A future retained across `poll` calls, used to expose an async operation as
/// a `poll_*` method.
///
/// The future is created on the first poll, kept while it is pending, and
/// dropped once it resolves so the next poll starts a fresh operation.
///
/// Retention is the whole point: a `poll_*` method that built a fresh future
/// every call would discard whatever the operation had already done before it
/// returned `Pending` — a claimed stream credit, a reserved queue slot, or a
/// waker registration that only the original future would be woken for.
///
/// Each slot drives one operation at a time. Cloning yields an idle slot; the
/// in-progress future stays with the original.
pub struct OpState<T> {
    // The Mutex is here so the enclosing type stays Sync: a boxed Send future
    // is not itself Sync. Polling goes through `&mut self` and uses `get_mut`,
    // so there is no runtime locking on that path.
    future: Mutex<Option<BoxFuture<T>>>,
}

impl<T> OpState<T> {
    /// Returns whether an operation is currently in progress.
    pub fn is_pending(&self) -> bool {
        self.future.lock().unwrap().is_some()
    }

    /// Abandon the operation in progress, if any.
    pub fn clear(&mut self) {
        *self.future.get_mut().unwrap() = None;
    }
}

impl<T: 'static> OpState<T> {
    /// Poll the operation in progress, starting one with `make` when idle.
    ///
    /// `make` is only called when there is no operation in progress, so the
    /// caller can build its argument lazily rather than on every poll.
    pub fn poll<F>(&mut self, cx: &mut Context<'_>, make: impl FnOnce() -> F) -> Poll<T>
    where
        F: Future<Output = T> + crate::MaybeSend + 'static,
    {
        let state = self.future.get_mut().unwrap();
        let future = state.get_or_insert_with(|| Box::pin(make()));
        let output = ready!(future.as_mut().poll(cx));
        *state = None;
        Poll::Ready(output)
    }
}

impl<T> Default for OpState<T> {
    fn default() -> Self {
        Self {
            future: Mutex::new(None),
        }
    }
}

impl<T> Clone for OpState<T> {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl<T> std::fmt::Debug for OpState<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpState")
            .field("pending", &self.is_pending())
            .finish()
    }
}

/// The retained futures a [`PollSession`](crate::PollSession) implementation needs for the
/// operations that have no native poll form.
///
/// Every backend today opens streams, reads datagrams and awaits close through
/// multi-step async routines, so each of those needs an [`OpState`] to be pollable.
/// Accept is usually the exception — most backends already drive it from a poll
/// loop — so the accept slots are there for those that don't.
///
/// Because `PollSession` operations take `&mut self`, each of these slots has exactly
/// one owner: there is no shared-slot contention and no need to clone the session
/// per call. Cloning this struct yields idle slots, matching [`OpState`].
pub struct SessionOps<S, R, E> {
    /// Retains `accept_uni` for backends without a native poll form.
    pub accept_uni: OpState<Result<R, E>>,
    /// Retains `accept_bi` for backends without a native poll form.
    pub accept_bi: OpState<Result<(S, R), E>>,
    /// Retains the in-progress `open_uni`, which claims stream credit before it
    /// resolves — restarting it would leak the claim.
    pub open_uni: OpState<Result<S, E>>,
    /// Retains the in-progress `open_bi`, which claims stream credit before it
    /// resolves — restarting it would leak the claim.
    pub open_bi: OpState<Result<(S, R), E>>,
    /// Retains `recv_datagram`.
    pub recv_datagram: OpState<Result<bytes::Bytes, E>>,
    /// Retains `closed`.
    pub closed: OpState<E>,
}

impl<S, R, E> Default for SessionOps<S, R, E> {
    fn default() -> Self {
        Self {
            accept_uni: Default::default(),
            accept_bi: Default::default(),
            open_uni: Default::default(),
            open_bi: Default::default(),
            recv_datagram: Default::default(),
            closed: Default::default(),
        }
    }
}

impl<S, R, E> Clone for SessionOps<S, R, E> {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl<S, R, E> std::fmt::Debug for SessionOps<S, R, E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("SessionOps").finish_non_exhaustive()
    }
}

/// A trait that is Send on native targets and empty on WASM.
#[cfg(not(target_family = "wasm"))]
pub trait MaybeSend: Send {}

/// A trait that is Sync on native targets and empty on WASM.
#[cfg(not(target_family = "wasm"))]
pub trait MaybeSync: Sync {}

#[cfg(not(target_family = "wasm"))]
impl<T: Send> MaybeSend for T {}

#[cfg(not(target_family = "wasm"))]
impl<T: Sync> MaybeSync for T {}

/// A trait that is Send on native targets and empty on WASM.
#[cfg(target_family = "wasm")]
pub trait MaybeSend {}

/// A trait that is Sync on native targets and empty on WASM.
#[cfg(target_family = "wasm")]
pub trait MaybeSync {}

#[cfg(target_family = "wasm")]
impl<T> MaybeSend for T {}

#[cfg(target_family = "wasm")]
impl<T> MaybeSync for T {}

#[cfg(test)]
mod tests {
    use std::{
        future::{pending, ready},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        task::{Context, Poll, Wake, Waker},
    };

    use super::OpState;

    struct Noop;

    impl Wake for Noop {
        fn wake(self: Arc<Self>) {}
    }

    fn waker() -> Waker {
        Waker::from(Arc::new(Noop))
    }

    /// The reason `OpState` holds a `Mutex`: a boxed `Send` future is not `Sync`,
    /// and embedding one in a public stream type would silently strip `Sync` from
    /// it. Every backend's `SendStream` was `Send + Sync` before this existed.
    #[test]
    fn op_state_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<OpState<()>>();
    }

    #[test]
    fn pending_future_is_retained() {
        let starts = Arc::new(AtomicUsize::new(0));
        let mut state = OpState::default();
        let waker = waker();
        let mut cx = Context::from_waker(&waker);

        for _ in 0..2 {
            let starts = starts.clone();
            assert!(state
                .poll(&mut cx, move || {
                    starts.fetch_add(1, Ordering::Relaxed);
                    pending::<()>()
                })
                .is_pending());
        }

        assert_eq!(starts.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn completed_future_is_cleared() {
        let mut state = OpState::default();
        let waker = waker();
        let mut cx = Context::from_waker(&waker);

        assert_eq!(state.poll(&mut cx, || ready(1)), Poll::Ready(1));
        assert!(!state.is_pending());
        assert_eq!(state.poll(&mut cx, || ready(2)), Poll::Ready(2));
    }

    #[test]
    fn cleared_future_restarts() {
        let mut state = OpState::default();
        let waker = waker();
        let mut cx = Context::from_waker(&waker);

        assert!(state.poll(&mut cx, pending::<()>).is_pending());
        state.clear();
        assert!(!state.is_pending());
    }

    #[test]
    fn clone_has_an_independent_idle_slot() {
        let mut state = OpState::default();
        let waker = waker();
        let mut cx = Context::from_waker(&waker);

        assert!(state.poll(&mut cx, pending::<()>).is_pending());

        let clone = state.clone();
        assert!(state.is_pending());
        assert!(!clone.is_pending());
    }
}
