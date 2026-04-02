use pyo3::prelude::*;

use crate::errors;
use crate::recv_stream::RecvStream;
use crate::runtime;
use crate::send_stream::SendStream;

#[pyclass]
pub struct Session {
    inner: web_transport_quinn::Session,
}

impl Session {
    pub fn new(session: web_transport_quinn::Session) -> Self {
        Self { inner: session }
    }
}

#[pymethods]
impl Session {
    fn __aenter__<'py>(slf: PyRef<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let obj: Py<PyAny> = slf.into_pyobject(py)?.into_any().unbind();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok(obj) })
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        _exc_type: Option<Py<PyAny>>,
        _exc_val: Option<Py<PyAny>>,
        _exc_tb: Option<Py<PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let session = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            session.close(0, b"");
            let _ = session.closed().await;
            Ok(())
        })
    }

    /// Open a new bidirectional stream.
    fn open_bi<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let session = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let (send, recv) = session.open_bi().await.map_err(errors::map_session_error)?;
            Ok((SendStream::new(send), RecvStream::new(recv)))
        })
    }

    /// Open a new unidirectional (send-only) stream.
    fn open_uni<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let session = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let send = session
                .open_uni()
                .await
                .map_err(errors::map_session_error)?;
            Ok(SendStream::new(send))
        })
    }

    /// Wait for the peer to open a bidirectional stream.
    fn accept_bi<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let session = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let (send, recv) = session
                .accept_bi()
                .await
                .map_err(errors::map_session_error)?;
            Ok((SendStream::new(send), RecvStream::new(recv)))
        })
    }

    /// Wait for the peer to open a unidirectional stream.
    fn accept_uni<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let session = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let recv = session
                .accept_uni()
                .await
                .map_err(errors::map_session_error)?;
            Ok(RecvStream::new(recv))
        })
    }

    /// Send an unreliable datagram.
    fn send_datagram(&self, data: Vec<u8>) -> PyResult<()> {
        self.inner
            .send_datagram(bytes::Bytes::from(data))
            .map_err(errors::map_session_error)?;
        Ok(())
    }

    /// Wait for and return the next incoming datagram.
    fn receive_datagram<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let session = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let data = session
                .read_datagram()
                .await
                .map_err(errors::map_session_error)?;
            Ok(data.to_vec())
        })
    }

    /// Close the session immediately.
    #[pyo3(signature = (code=0, reason=""))]
    fn close(&self, code: u32, reason: &str) {
        let _guard = runtime::get_runtime().enter();
        self.inner.close(code, reason.as_bytes());
    }

    /// Wait until the session is closed (for any reason).
    fn wait_closed<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let session = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let _ = session.closed().await;
            Ok(())
        })
    }

    /// The reason the session was closed, or None if still open.
    #[getter]
    fn close_reason(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.inner.close_reason().map(|err| {
            errors::map_session_error(err)
                .value(py)
                .as_any()
                .clone()
                .unbind()
        })
    }

    /// Maximum payload size for send_datagram.
    #[getter]
    fn max_datagram_size(&self) -> usize {
        self.inner.max_datagram_size()
    }

    /// The remote peer's ``(host, port)``.
    #[getter]
    fn remote_address(&self) -> (String, u16) {
        let addr = self.inner.remote_address();
        (addr.ip().to_string(), addr.port())
    }

    /// Current estimated round-trip time in seconds.
    #[getter]
    fn rtt(&self) -> f64 {
        self.inner.rtt().as_secs_f64()
    }
}
