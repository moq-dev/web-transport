//! Shared tokio runtime used by every UniFFI-exported async method.
//!
//! A single current-thread runtime lives on a dedicated worker thread,
//! accessible via [`RUNTIME.enter()`] (to register quinn timers from `Drop`
//! paths) and via [`RUNTIME.spawn()`] (to drive futures returned from
//! `#[uniffi::export] async fn` methods).

use std::sync::LazyLock;

pub(crate) static RUNTIME: LazyLock<tokio::runtime::Handle> = LazyLock::new(|| {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.expect("failed to build web-transport-ffi runtime");
	let handle = runtime.handle().clone();

	std::thread::Builder::new()
		.name("web-transport-ffi".into())
		.spawn(move || {
			runtime.block_on(std::future::pending::<()>());
		})
		.expect("failed to spawn web-transport-ffi runtime thread");

	handle
});
