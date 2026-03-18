use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use pyo3::exceptions::PyStopAsyncIteration;
use pyo3::prelude::*;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

use crate::errors;

#[pyclass]
pub struct RecvStream {
    inner: Arc<Mutex<web_transport_quinn::RecvStream>>,
    cancel: CancellationToken,
    stop_code: Arc<AtomicU32>,
    eof: Arc<AtomicBool>,
}

impl RecvStream {
    pub fn new(stream: web_transport_quinn::RecvStream) -> Self {
        Self {
            inner: Arc::new(Mutex::new(stream)),
            cancel: CancellationToken::new(),
            stop_code: Arc::new(AtomicU32::new(0)),
            eof: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Run a read operation with cancellation support.
    ///
    /// `read_fn` receives the stream mutex and returns a future that performs
    /// the actual read.  If the cancellation token fires while that future is
    /// pending, it is dropped (releasing any held locks), STOP_SENDING is sent
    /// on the QUIC stream, and `StreamClosedLocally` is returned.
    fn cancellable_read<'py, T, F, Fut>(
        &self,
        py: Python<'py>,
        read_fn: F,
    ) -> PyResult<Bound<'py, PyAny>>
    where
        T: for<'a> pyo3::IntoPyObject<'a> + Send + 'static,
        F: FnOnce(OwnedMutexGuard<web_transport_quinn::RecvStream>) -> Fut + Send + 'static,
        Fut: Future<Output = PyResult<T>> + Send + 'static,
    {
        let inner = self.inner.clone();
        let cancel = self.cancel.clone();
        let stop_code = self.stop_code.clone();
        let stream = inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let mut guard = inner.lock().await;
                    let code = stop_code.load(Ordering::Acquire);
                    let _ = guard.stop(code);
                    Err(errors::StreamClosedLocally::new_err("stream stopped locally"))
                }
                result = async {
                    let guard = stream.lock_owned().await;
                    read_fn(guard).await
                } => {
                    result
                }
            }
        })
    }
}

#[pymethods]
impl RecvStream {
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
        let stop_code = self.stop_code.clone();
        let eof = self.eof.clone();
        let has_exception = exc_type.is_some();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            if has_exception {
                // Cancel first to interrupt any in-progress read, then lock.
                stop_code.store(0, Ordering::Release);
                cancel.cancel();
                let mut guard = inner.lock().await;
                let _ = guard.stop(0);
            } else if !eof.load(Ordering::Acquire) {
                // Clean exit but not at EOF — promptly signal the peer.
                let mut guard = inner.lock().await;
                let _ = guard.stop(0);
            }
            Ok(())
        })
    }

    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let eof = self.eof.clone();
        self.cancellable_read(py, |mut guard| async move {
            if eof.load(Ordering::Acquire) {
                return Err(PyStopAsyncIteration::new_err(()));
            }
            let chunk = guard
                .read_chunk(65536, true)
                .await
                .map_err(errors::map_read_error)?;
            match chunk {
                Some(chunk) => Ok(chunk.bytes.to_vec()),
                None => {
                    eof.store(true, Ordering::Release);
                    Err(PyStopAsyncIteration::new_err(()))
                }
            }
        })
    }

    /// Read up to *n* bytes, or until EOF if *n* is ``-1``.
    #[pyo3(signature = (n=-1, *, limit=None))]
    fn read<'py>(
        &self,
        py: Python<'py>,
        n: i64,
        limit: Option<usize>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let eof = self.eof.clone();
        self.cancellable_read(py, move |mut guard| async move {
            if eof.load(Ordering::Acquire) {
                return Ok(Vec::new());
            }
            if n < 0 {
                let limit = limit.unwrap_or(usize::MAX);
                let data = guard
                    .read_to_end(limit)
                    .await
                    .map_err(|e| errors::map_read_to_end_error(e, limit))?;
                eof.store(true, Ordering::Release);
                Ok(data)
            } else if n == 0 {
                Ok(Vec::new())
            } else {
                let chunk = guard
                    .read_chunk(n as usize, true)
                    .await
                    .map_err(errors::map_read_error)?;
                match chunk {
                    Some(chunk) => Ok(chunk.bytes.to_vec()),
                    None => {
                        eof.store(true, Ordering::Release);
                        Ok(Vec::new())
                    }
                }
            }
        })
    }

    /// Read exactly *n* bytes.
    fn readexactly<'py>(&self, py: Python<'py>, n: usize) -> PyResult<Bound<'py, PyAny>> {
        let eof = self.eof.clone();
        self.cancellable_read(py, move |mut guard| async move {
            if n > 0 && eof.load(Ordering::Acquire) {
                return Err(errors::new_stream_incomplete_read_error(n, &[]));
            }
            let mut buf = vec![0u8; n];
            match guard.read_exact(&mut buf).await {
                Ok(()) => Ok(buf),
                Err(e) => {
                    if matches!(e, web_transport_quinn::ReadExactError::FinishedEarly(_)) {
                        eof.store(true, Ordering::Release);
                    }
                    Err(errors::map_read_exact_error(e, n, &buf))
                }
            }
        })
    }

    /// Tell the peer to stop sending on this stream.
    #[pyo3(signature = (error_code=0))]
    fn stop(&self, error_code: u32) -> PyResult<()> {
        if self.cancel.is_cancelled() {
            return Err(errors::StreamClosedLocally::new_err(
                "stream already closed",
            ));
        }
        self.stop_code.store(error_code, Ordering::Release);
        self.cancel.cancel();
        // If no read is in progress, send STOP_SENDING immediately.
        // Otherwise the interrupted read will send it when it wakes.
        if let Ok(mut guard) = self.inner.try_lock() {
            let _ = guard.stop(error_code);
        }
        Ok(())
    }

    /// Wait until the peer resets the stream or it is otherwise closed.
    fn wait_closed<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.cancellable_read(py, |mut guard| async move {
            guard
                .received_reset()
                .await
                .map_err(errors::map_session_error)
        })
    }
}
