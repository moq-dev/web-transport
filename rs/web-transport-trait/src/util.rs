//! Initialized buffers and utility traits for conditional Send/Sync bounds.
//!
//! These traits allow the same code to work on both native and WASM targets,
//! where WASM doesn't support Send/Sync.

pub(crate) const READ_CAPACITY: usize = 8 * 1024;

/// Initialize spare capacity before exposing it to a safe reader.
pub(crate) fn initialize(dst: &mut bytes::buf::UninitSlice) -> &mut [u8] {
    // Bound initialization independently of allocation size and read progress.
    let len = dst.len().min(READ_CAPACITY);
    let dst = &mut dst[..len];
    for index in 0..dst.len() {
        dst.write_byte(index, 0);
    }

    // SAFETY: Every byte was initialized above, and the slice retains the
    // exclusive borrow and bounds of the original destination.
    unsafe { std::slice::from_raw_parts_mut(dst.as_mut_ptr(), dst.len()) }
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
