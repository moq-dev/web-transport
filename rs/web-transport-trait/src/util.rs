//! Utility traits for conditional Send/Sync bounds.
//!
//! These traits allow the same code to work on both native and WASM targets,
//! where WASM doesn't support Send/Sync.

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
