//! The browser plumbing shared by the session and its streams.
//!
//! Every WebTransport operation in the browser is a promise, and a promise delivers
//! its result exactly once to whoever is subscribed at the time. [`JsFuture`]
//! subscribes when it is built and owns the slot the result lands in, so dropping
//! one between polls throws that result away — for a `read()` that is an accepted
//! stream or a chunk of stream data, gone with no way to ask for it again. The
//! future therefore has to outlive the poll that created it, which is what [`Op`]
//! is for.

use std::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
};

use js_sys::Promise;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::ReadableStreamReadResult;

use crate::Error;

/// Erase a promise's resolved type so one slot type serves every operation.
///
/// `web-sys` types its promises from 0.3.103 onward but not before, and this crate
/// supports both; going through [`JsValue`] is the shape that compiles either way.
pub(crate) fn promise(value: impl Into<JsValue>) -> Promise {
    value.into().unchecked_into()
}

/// Swallow a promise's outcome, including a rejection.
///
/// `close`, `abort` and `cancel` all reject when the stream is already gone, which
/// is exactly when there is nothing left to do about it. The promise still has to
/// be consumed: an *unhandled* rejection is a console error.
pub(crate) fn ignore(value: impl Into<JsValue>) {
    let promise = promise(value);
    wasm_bindgen_futures::spawn_local(async move {
        let _ = JsFuture::from(promise).await;
    });
}

/// The value from a `read()`, or `None` once the stream is done.
pub(crate) fn read_value<T: JsCast>(result: JsValue) -> Option<T> {
    let result: ReadableStreamReadResult = result.unchecked_into();

    if result.get_done().unwrap_or(false) {
        return None;
    }

    Some(result.get_value().unchecked_into())
}

/// One in-flight browser operation.
///
/// The promise is subscribed to on first poll, kept while it is pending, and
/// dropped once it settles so the next poll starts a fresh operation.
///
/// The slot is a [`RefCell`] because this crate's async methods take `&self` while
/// the poll traits take `&mut self`; both borrow through it. There is nothing to
/// contend over — the browser runs this on one thread.
#[derive(Default)]
pub(crate) struct Op {
    future: RefCell<Option<JsFuture>>,
}

impl Op {
    /// Poll the operation in flight, starting one with `start` when idle.
    ///
    /// `start` is only called when the slot is empty, so a caller can build its
    /// arguments lazily rather than on every poll.
    pub(crate) fn poll(
        &self,
        cx: &mut Context<'_>,
        start: impl FnOnce() -> Promise,
    ) -> Poll<Result<JsValue, Error>> {
        self.poll_with(cx, || JsFuture::from(start()))
    }

    /// Poll the operation in flight, taking `make` for one when idle.
    ///
    /// Lazy in `make`, which is what lets a caller adopt a future orphaned by a
    /// dropped handle instead of asking the browser for a fresh one.
    pub(crate) fn poll_with(
        &self,
        cx: &mut Context<'_>,
        make: impl FnOnce() -> JsFuture,
    ) -> Poll<Result<JsValue, Error>> {
        let mut slot = self.future.borrow_mut();
        let future = slot.get_or_insert_with(make);
        let output = ready!(Pin::new(future).poll(cx));
        *slot = None;
        Poll::Ready(output.map_err(Error::from))
    }

    /// Take the operation in flight, leaving the slot idle.
    pub(crate) fn take(&self) -> Option<JsFuture> {
        self.future.borrow_mut().take()
    }

    /// Replace the operation in flight, returning its predecessor when there was one.
    pub(crate) fn replace(&self, future: JsFuture) -> Option<JsFuture> {
        self.future.borrow_mut().replace(future)
    }

    /// Poll an operation already in flight, reporting an idle slot as settled.
    ///
    /// Unlike [`poll`](Self::poll) this never starts one, which is what lets a
    /// writer wait for the *previous* write before it touches the caller's buffer.
    pub(crate) fn poll_settled(&self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        let mut slot = self.future.borrow_mut();
        let Some(future) = slot.as_mut() else {
            return Poll::Ready(Ok(()));
        };

        let output = ready!(Pin::new(future).poll(cx));
        *slot = None;
        Poll::Ready(output.map(|_| ()).map_err(Error::from))
    }

    /// Put an operation in flight without polling it, replacing any predecessor.
    ///
    /// Dropping the replaced future is safe: its callbacks are already attached to
    /// the promise and self-contained, so the outcome is discarded rather than
    /// surfacing as an unhandled rejection.
    pub(crate) fn start(&self, promise: Promise) {
        *self.future.borrow_mut() = Some(JsFuture::from(promise));
    }
}

/// A clone starts idle: each handle owns its own progress under the poll contract,
/// and the operation in flight belongs to the handle that started it.
impl Clone for Op {
    fn clone(&self) -> Self {
        Self::default()
    }
}
