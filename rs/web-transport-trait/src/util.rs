//! Utilities for conditional Send/Sync bounds and persistent poll operations.
//!
//! These traits allow the same code to work on both native and WASM targets,
//! where WASM doesn't support Send/Sync.

use std::{
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
};

#[cfg(not(target_family = "wasm"))]
type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

#[cfg(target_family = "wasm")]
type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + 'static>>;

/// Persistent state for an operation exposed by a poll-based API.
///
/// The future is created lazily, retained while pending, and cleared after it
/// resolves so the next poll starts a fresh operation.
pub struct OpState<T> {
    future: std::sync::Mutex<Option<BoxFuture<T>>>,
}

impl<T> std::fmt::Debug for OpState<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpState")
            .field("pending", &self.is_pending())
            .finish()
    }
}

impl<T> Default for OpState<T> {
    fn default() -> Self {
        Self {
            future: std::sync::Mutex::new(None),
        }
    }
}

impl<T> OpState<T> {
    /// Returns whether an operation is currently pending.
    pub fn is_pending(&self) -> bool {
        self.future.lock().unwrap().is_some()
    }

    /// Drop an in-progress operation.
    pub fn clear(&mut self) {
        *self.future.get_mut().unwrap() = None;
    }
}

#[cfg(not(target_family = "wasm"))]
impl<T: 'static> OpState<T> {
    /// Poll the current operation, creating it with `make` when idle.
    pub fn poll<F>(&mut self, cx: &mut Context<'_>, make: impl FnOnce() -> F) -> Poll<T>
    where
        F: Future<Output = T> + Send + 'static,
    {
        let state = self.future.get_mut().unwrap();
        let future = state.get_or_insert_with(|| Box::pin(make()));
        let output = ready!(future.as_mut().poll(cx));
        *state = None;
        Poll::Ready(output)
    }
}

#[cfg(target_family = "wasm")]
impl<T: 'static> OpState<T> {
    /// Poll the current operation, creating it with `make` when idle.
    pub fn poll<F>(&mut self, cx: &mut Context<'_>, make: impl FnOnce() -> F) -> Poll<T>
    where
        F: Future<Output = T> + 'static,
    {
        let state = self.future.get_mut().unwrap();
        let future = state.get_or_insert_with(|| Box::pin(make()));
        let output = ready!(future.as_mut().poll(cx));
        *state = None;
        Poll::Ready(output)
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
