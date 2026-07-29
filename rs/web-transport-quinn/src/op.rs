//! A future retained across `poll` calls.
//!
//! Quinn exposes most connection-level operations only as futures, and those
//! futures hold the thing that matters between polls: the `Notify` registration
//! that will wake us. Building a fresh one inside every `poll_*` call would drop
//! that registration the moment we returned `Pending`, and nothing would ever wake
//! the caller again. So the future has to outlive the poll that created it.
//!
//! This is deliberately private to this crate. It boxes a `Send` future, which a
//! transport pinned to one thread could not use, so it has no business in
//! `web-transport-trait` — the poll traits describe what an implementation must do,
//! not how a runtime-backed one gets there.

use std::{
    future::Future,
    pin::Pin,
    sync::Mutex,
    task::{ready, Context, Poll},
};

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// One in-flight operation.
///
/// The future is created on first poll, kept while it is pending, and dropped once
/// it resolves so the next poll starts a fresh operation. Cloning yields an idle
/// slot: the in-progress operation stays with the handle that started it, which is
/// what makes cloning a session the way to run operations concurrently.
pub(crate) struct Op<T> {
    // The `Mutex` is here only so the enclosing `Session` stays `Sync`: a boxed
    // `Send` future is not itself `Sync`. Polling goes through `&mut self` and uses
    // `get_mut`, so there is no runtime locking on that path.
    future: Mutex<Option<BoxFuture<T>>>,
}

impl<T: 'static> Op<T> {
    /// Poll the operation in progress, starting one with `make` when idle.
    ///
    /// `make` is only called when there is nothing in flight, so the caller can
    /// build its arguments lazily rather than on every poll.
    pub(crate) fn poll<F>(&mut self, cx: &mut Context<'_>, make: impl FnOnce() -> F) -> Poll<T>
    where
        F: Future<Output = T> + Send + 'static,
    {
        let slot = self.future.get_mut().unwrap();
        let future = slot.get_or_insert_with(|| Box::pin(make()));
        let output = ready!(future.as_mut().poll(cx));
        *slot = None;
        Poll::Ready(output)
    }
}

impl<T> Default for Op<T> {
    fn default() -> Self {
        Self {
            future: Mutex::new(None),
        }
    }
}

impl<T> Clone for Op<T> {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl<T> std::fmt::Debug for Op<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Op").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::Op;

    /// The reason `Op` holds a `Mutex`. Every backend `Session` was `Send + Sync`
    /// before this existed, and embedding a bare boxed future would have silently
    /// taken `Sync` away.
    #[test]
    fn op_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Op<()>>();
    }
}
