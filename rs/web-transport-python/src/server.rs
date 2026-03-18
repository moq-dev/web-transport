use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use pyo3::exceptions::{PyStopAsyncIteration, PyValueError};
use pyo3::prelude::*;
use tokio::sync::Mutex;

use crate::errors;
use crate::runtime;
use crate::session::Session;

#[pyclass]
pub struct Server {
    inner: Arc<Mutex<web_transport_quinn::Server>>,
    endpoint: quinn::Endpoint,
    local_addr: (String, u16),
}

#[pymethods]
impl Server {
    #[new]
    #[pyo3(signature = (*, certificate_chain, private_key, bind="[::]:4433", congestion_control="default", max_idle_timeout=Some(30.0), keep_alive_interval=None))]
    fn new(
        certificate_chain: Vec<Vec<u8>>,
        private_key: Vec<u8>,
        bind: &str,
        congestion_control: &str,
        max_idle_timeout: Option<f64>,
        keep_alive_interval: Option<f64>,
    ) -> PyResult<Self> {
        let addr: SocketAddr = bind
            .parse()
            .map_err(|e| PyValueError::new_err(format!("invalid bind address: {e}")))?;

        // Build TLS config
        let certs: Vec<rustls::pki_types::CertificateDer<'static>> = certificate_chain
            .into_iter()
            .map(rustls::pki_types::CertificateDer::from)
            .collect();

        let key = rustls::pki_types::PrivateKeyDer::try_from(private_key)
            .map_err(|e| PyValueError::new_err(format!("invalid private key: {e}")))?;

        let provider = rustls::crypto::ring::default_provider();

        let mut tls_config = rustls::ServerConfig::builder_with_provider(Arc::new(provider))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| PyValueError::new_err(format!("TLS config error: {e}")))?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| PyValueError::new_err(format!("certificate error: {e}")))?;

        tls_config.alpn_protocols = vec![web_transport_quinn::ALPN.as_bytes().to_vec()];

        // Build transport config
        let mut transport = quinn::TransportConfig::default();
        transport.max_idle_timeout(
            max_idle_timeout
                .map(Duration::try_from_secs_f64)
                .transpose()
                .map_err(|_| PyValueError::new_err("invalid max_idle_timeout"))?
                .map(quinn::IdleTimeout::try_from)
                .transpose()
                .map_err(|e| PyValueError::new_err(format!("invalid idle timeout: {e}")))?,
        );
        transport.keep_alive_interval(
            keep_alive_interval
                .map(Duration::try_from_secs_f64)
                .transpose()
                .map_err(|_| PyValueError::new_err("invalid keep_alive_interval"))?,
        );

        // Congestion control — matches ClientBuilder::with_congestion_control()
        let congestion_controller: Option<
            Arc<dyn quinn::congestion::ControllerFactory + Send + Sync + 'static>,
        > = match congestion_control {
            "default" => None,
            "throughput" => Some(Arc::new(quinn::congestion::CubicConfig::default())),
            "low_latency" => Some(Arc::new(quinn::congestion::BbrConfig::default())),
            other => {
                return Err(PyValueError::new_err(format!(
                    "unknown congestion control: {other}"
                )));
            }
        };

        if let Some(cc) = congestion_controller {
            transport.congestion_controller_factory(cc.clone());
        }

        // Build quinn server config
        let quic_config = quinn::crypto::rustls::QuicServerConfig::try_from(tls_config)
            .map_err(|e| PyValueError::new_err(format!("QUIC config error: {e}")))?;
        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_config));
        server_config.transport_config(Arc::new(transport));

        // Bind endpoint
        let _guard = runtime::get_runtime().enter();
        let endpoint = quinn::Endpoint::server(server_config, addr)
            .map_err(|e| PyValueError::new_err(format!("failed to bind: {e}")))?;

        let local_addr = endpoint
            .local_addr()
            .map_err(|e| PyValueError::new_err(format!("failed to get local addr: {e}")))?;

        let server = web_transport_quinn::Server::new(endpoint.clone());

        Ok(Self {
            inner: Arc::new(Mutex::new(server)),
            endpoint,
            local_addr: (local_addr.ip().to_string(), local_addr.port()),
        })
    }

    fn __aenter__<'py>(slf: PyRef<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let obj: Py<PyAny> = slf.into_pyobject(py)?.into_any().unbind();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok(obj) })
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __aexit__<'py>(
        &mut self,
        py: Python<'py>,
        _exc_type: Option<Py<PyAny>>,
        _exc_val: Option<Py<PyAny>>,
        _exc_tb: Option<Py<PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // Close and wait for idle
        self.endpoint.close(quinn::VarInt::from_u32(0), b"");
        let endpoint = self.endpoint.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            endpoint.wait_idle().await;
            Ok(())
        })
    }

    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = inner.lock().await;
            match guard.accept().await {
                Some(request) => Ok(SessionRequest::new(request)),
                None => Err(PyStopAsyncIteration::new_err(())),
            }
        })
    }

    /// Wait for the next incoming session request.
    fn accept<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = inner.lock().await;
            match guard.accept().await {
                Some(request) => Ok(Some(SessionRequest::new(request))),
                None => Ok(None),
            }
        })
    }

    /// Close all connections immediately.
    #[pyo3(signature = (code=0, reason=""))]
    fn close(&self, code: u64, reason: &str) -> PyResult<()> {
        let var_code = quinn::VarInt::from_u64(code)
            .map_err(|_| PyValueError::new_err("code must be less than 2**62"))?;
        self.endpoint.close(var_code, reason.as_bytes());
        Ok(())
    }

    /// Wait for all connections to be cleanly shut down.
    fn wait_closed<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let endpoint = self.endpoint.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            endpoint.wait_idle().await;
            Ok(())
        })
    }

    /// The local ``(host, port)`` the server is bound to.
    #[getter]
    fn local_addr(&self) -> (String, u16) {
        self.local_addr.clone()
    }
}

// ---------------------------------------------------------------------------
// SessionRequest
// ---------------------------------------------------------------------------

#[pyclass]
pub struct SessionRequest {
    inner: Option<web_transport_quinn::Request>,
    url: String,
}

impl SessionRequest {
    pub fn new(request: web_transport_quinn::Request) -> Self {
        let url = request.url.to_string();
        Self {
            inner: Some(request),
            url,
        }
    }
}

#[pymethods]
impl SessionRequest {
    /// The URL requested by the client.
    #[getter]
    fn url(&self) -> String {
        self.url.clone()
    }

    /// Accept the session request.
    fn accept<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let request = self
            .inner
            .take()
            .ok_or_else(|| errors::SessionError::new_err("request already accepted or rejected"))?;
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let session = request.ok().await.map_err(errors::map_server_error)?;
            Ok(Session::new(session))
        })
    }

    /// Reject the session request with an HTTP status code.
    #[pyo3(signature = (status_code=404))]
    fn reject<'py>(&mut self, py: Python<'py>, status_code: u16) -> PyResult<Bound<'py, PyAny>> {
        let request = self
            .inner
            .take()
            .ok_or_else(|| errors::SessionError::new_err("request already accepted or rejected"))?;
        let status = http::StatusCode::from_u16(status_code)
            .map_err(|e| PyValueError::new_err(format!("invalid status code: {e}")))?;
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            // Use respond() instead of reject() so we get back a Session that
            // keeps the QUIC connection alive.  reject() drops the connection
            // immediately, which can trigger an implicit CONNECTION_CLOSE
            // before the rejection response is transmitted to the peer.
            let session = request
                .respond(status)
                .await
                .map_err(errors::map_server_error)?;
            // Close from our side so we don't block on client behavior.
            // The QUIC draining period ensures the rejection response is
            // transmitted alongside the CONNECTION_CLOSE.
            session.close(0, b"");
            session.closed().await;
            Ok(())
        })
    }
}
