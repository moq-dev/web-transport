use std::future::Future;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::Arc;

use pyo3::prelude::*;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

use crate::errors;

#[pyclass]
pub struct SendStream {
    inner: Arc<Mutex<web_transport_quinn::SendStream>>,
    cancel: CancellationToken,
    reset_code: Arc<AtomicU32>,
    priority: Arc<AtomicI32>,
    synced_priority: Arc<AtomicI32>,
}

impl SendStream {
    pub fn new(stream: web_transport_quinn::SendStream) -> Self {
        let priority = stream.priority().unwrap_or(0);
        Self {
            inner: Arc::new(Mutex::new(stream)),
            cancel: CancellationToken::new(),
            reset_code: Arc::new(AtomicU32::new(0)),
            priority: Arc::new(AtomicI32::new(priority)),
            synced_priority: Arc::new(AtomicI32::new(priority)),
        }
    }

    /// Run a write operation with cancellation support.
    ///
    /// If `reset()` fires while `write_fn` is pending, the future is dropped
    /// (releasing any held locks), RESET_STREAM is sent on the QUIC stream,
    /// and `StreamClosedLocally` is returned.
    fn cancellable_write<'py, T, F, Fut>(
        &self,
        py: Python<'py>,
        write_fn: F,
    ) -> PyResult<Bound<'py, PyAny>>
    where
        T: for<'a> pyo3::IntoPyObject<'a> + Send + 'static,
        F: FnOnce(OwnedMutexGuard<web_transport_quinn::SendStream>) -> Fut + Send + 'static,
        Fut: Future<Output = PyResult<T>> + Send + 'static,
    {
        let inner = self.inner.clone();
        let cancel = self.cancel.clone();
        let reset_code = self.reset_code.clone();
        let priority = self.priority.clone();
        let synced_priority = self.synced_priority.clone();
        let stream = inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let mut guard = inner.lock().await;
                    let code = reset_code.load(Ordering::Acquire);
                    let _ = guard.reset(code);
                    Err(errors::StreamClosedLocally::new_err("stream reset locally"))
                }
                result = async {
                    let guard = stream.lock_owned().await;
                    let desired = priority.load(Ordering::Relaxed);
                    if desired != synced_priority.load(Ordering::Relaxed) {
                        let _ = guard.set_priority(desired);
                        synced_priority.store(desired, Ordering::Relaxed);
                    }
                    write_fn(guard).await
                } => {
                    result
                }
            }
        })
    }
}

#[pymethods]
impl SendStream {
    fn __aenter__<'py>(slf: PyRef<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let obj: Py<PyAny> = slf.into_pyobject(py)?.into_any().unbind();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok(obj) })
    }

    #[pyo3(signature = (exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        exc_type: Option<Py<PyAny>>,
        _exc_val: Option<Py<PyAny>>,
        _exc_tb: Option<Py<PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let cancel = self.cancel.clone();
        let reset_code = self.reset_code.clone();
        let has_exception = exc_type.is_some();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            if has_exception {
                // Cancel first to interrupt any in-progress write, then lock.
                reset_code.store(0, Ordering::Release);
                cancel.cancel();
                let mut guard = inner.lock().await;
                let _ = guard.reset(0);
            } else {
                let mut guard = inner.lock().await;
                let _ = guard.finish();
            }
            Ok(())
        })
    }

    /// Write all data to the stream.
    fn write<'py>(&self, py: Python<'py>, data: Vec<u8>) -> PyResult<Bound<'py, PyAny>> {
        self.cancellable_write(py, |mut guard| async move {
            guard
                .write_all(&data)
                .await
                .map_err(errors::map_write_error)?;
            Ok(())
        })
    }

    /// Write some data, returning the number of bytes written.
    fn write_some<'py>(&self, py: Python<'py>, data: Vec<u8>) -> PyResult<Bound<'py, PyAny>> {
        self.cancellable_write(py, |mut guard| async move {
            let n = guard.write(&data).await.map_err(errors::map_write_error)?;
            Ok(n)
        })
    }

    /// Gracefully close the stream, signaling EOF to the peer.
    ///
    /// Waits for any in-progress write to complete before sending FIN.
    /// Can be interrupted by `reset()`, which cancels both the pending
    /// write and this finish.
    fn finish<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.cancellable_write(py, |mut guard| async move {
            // ClosedStream from finish() means a prior finish() or reset()
            // call already moved the stream out of Ready — always local.
            guard
                .finish()
                .map_err(|_| errors::StreamClosedLocally::new_err("stream already closed"))
        })
    }

    /// Abruptly reset the stream.
    #[pyo3(signature = (error_code=0))]
    fn reset(&self, error_code: u32) -> PyResult<()> {
        if self.cancel.is_cancelled() {
            return Err(errors::StreamClosedLocally::new_err(
                "stream already closed",
            ));
        }
        self.reset_code.store(error_code, Ordering::Release);
        self.cancel.cancel();
        // If no write/finish is in progress, send RESET_STREAM immediately.
        // Otherwise the interrupted operation will send it when it wakes.
        if let Ok(mut guard) = self.inner.try_lock() {
            let _ = guard.reset(error_code);
        }
        Ok(())
    }

    /// Wait until the peer stops the stream or reads it to completion.
    fn wait_closed<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.cancellable_write(py, |guard| async move {
            guard.stopped().await.map_err(errors::map_session_error)
        })
    }

    /// Stream scheduling priority (higher = higher priority).
    #[getter]
    fn priority(&self) -> i32 {
        self.priority.load(Ordering::Relaxed)
    }

    #[setter]
    fn set_priority(&self, value: i32) {
        self.priority.store(value, Ordering::Relaxed);
        if let Ok(guard) = self.inner.try_lock() {
            let _ = guard.set_priority(value);
            self.synced_priority.store(value, Ordering::Relaxed);
        }
    }
}
