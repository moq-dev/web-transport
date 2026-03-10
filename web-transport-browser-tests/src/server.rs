use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::cert::TestCert;

type TaskHandles = Arc<Mutex<Vec<JoinHandle<()>>>>;

/// A boxed async handler invoked for each accepted WebTransport session.
pub type ServerHandler = Box<
    dyn FnMut(web_transport_quinn::Session) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + 'static,
>;

/// A running WebTransport test server.
pub struct TestServer {
    pub addr: SocketAddr,
    pub url: String,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    handler_tasks: TaskHandles,
}

impl TestServer {
    /// Shut down the server, verify exactly `expected_handlers` ran, and re-panic
    /// if any handler panicked.
    pub async fn shutdown(mut self, expected_handlers: usize) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.await.expect("accept loop task panicked");
        }
        let handles: Vec<_> = self.handler_tasks.lock().unwrap().drain(..).collect();
        assert_eq!(
            handles.len(),
            expected_handlers,
            "expected {expected_handlers} handler invocation(s), got {}",
            handles.len()
        );
        for (i, handle) in handles.into_iter().enumerate() {
            let result = tokio::time::timeout(Duration::from_secs(5), handle)
                .await
                .unwrap_or_else(|_| panic!("handler {i} did not complete within 5s"));
            if let Err(e) = result {
                if e.is_panic() {
                    std::panic::resume_unwind(e.into_panic());
                }
            }
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Start a WebTransport server on a random port using the given certificate and handler.
pub async fn start(cert: &TestCert, handler: ServerHandler) -> Result<TestServer> {
    let addr: SocketAddr = "[::1]:0".parse().unwrap();

    let server = web_transport_quinn::ServerBuilder::new()
        .with_addr(addr)
        .with_certificate(cert.chain.clone(), cert.key.clone_key())?;

    let actual_addr = server.local_addr()?;
    let url = format!("https://localhost:{}", actual_addr.port());

    tracing::debug!(%url, "test server listening");

    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let handler_tasks: TaskHandles = Arc::new(Mutex::new(Vec::new()));
    let handler_tasks2 = handler_tasks.clone();

    let task = tokio::spawn(async move {
        let mut server = server;
        let mut handler = handler;
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                request = server.accept() => {
                    let Some(request) = request else { break };
                    let session = request.ok().await
                        .expect("failed to accept session");
                    let fut = handler(session);
                    let handle = tokio::spawn(fut);
                    handler_tasks2.lock().unwrap().push(handle);
                }
            }
        }
    });

    Ok(TestServer {
        addr: actual_addr,
        url,
        shutdown_tx: Some(shutdown_tx),
        task: Some(task),
        handler_tasks,
    })
}

/// A handler that echoes bidirectional streams back to the client.
pub fn echo_handler() -> ServerHandler {
    Box::new(|session| {
        Box::pin(async move {
            echo_session(session).await;
        })
    })
}

async fn echo_session(session: web_transport_quinn::Session) {
    use tokio::io::AsyncWriteExt;

    let mut tasks = tokio::task::JoinSet::new();

    loop {
        tokio::select! {
            stream = session.accept_bi() => {
                match stream {
                    Ok((mut send, mut recv)) => {
                        tasks.spawn(async move {
                            let buf = recv.read_to_end(1024 * 1024).await
                                .expect("echo: read_to_end failed");
                            send.write_all(&buf).await
                                .expect("echo: write_all failed");
                            send.shutdown().await
                                .expect("echo: shutdown failed");
                        });
                    }
                    Err(e) if is_session_closed(&e) => break,
                    Err(e) => panic!("echo: accept_bi failed unexpectedly: {e}"),
                }
            }
            datagram = session.read_datagram() => {
                match datagram {
                    Ok(data) => session.send_datagram(data)
                        .expect("echo: send_datagram failed"),
                    Err(e) if is_session_closed(&e) => break,
                    Err(e) => panic!("echo: read_datagram failed unexpectedly: {e}"),
                }
            }
            Some(result) = tasks.join_next() => {
                if let Err(e) = result {
                    if e.is_panic() {
                        std::panic::resume_unwind(e.into_panic());
                    }
                }
            }
        }
    }

    // Drain remaining stream tasks and propagate panics.
    while let Some(result) = tasks.join_next().await {
        if let Err(e) = result {
            if e.is_panic() {
                std::panic::resume_unwind(e.into_panic());
            }
        }
    }
}

/// Returns true if the error indicates the session was closed (locally or by the peer).
pub fn is_session_closed(e: &web_transport_quinn::SessionError) -> bool {
    use web_transport_quinn::SessionError::*;
    matches!(
        e,
        ConnectionError(web_transport_quinn::quinn::ConnectionError::ApplicationClosed(_))
            | ConnectionError(web_transport_quinn::quinn::ConnectionError::LocallyClosed)
            | WebTransportError(web_transport_quinn::WebTransportError::Closed(_, _))
    )
}

/// A handler that accepts the session and immediately closes it.
pub fn immediate_close_handler(code: u32, reason: &'static str) -> ServerHandler {
    Box::new(move |session| {
        Box::pin(async move {
            // Give the browser a bit of time to finish establishing the session.
            // Without this, browser sometimes throws WebTransportError instead of
            // cleanly closing the session.
            tokio::time::sleep(Duration::from_millis(1000)).await;
            session.close(code, reason.as_bytes());
            // Wait for the connection to actually close.
            // This ensures the CloseWebTransportSession capsule is delivered.
            session.closed().await;
        })
    })
}

/// A handler that accepts the session and holds it open until the client disconnects.
pub fn idle_handler() -> ServerHandler {
    Box::new(|session| {
        Box::pin(async move {
            let err = session.closed().await;
            assert!(
                is_session_closed(&err),
                "idle: unexpected session error: {err}"
            );
        })
    })
}

/// A boxed async handler invoked with the raw [web_transport_quinn::Request] before
/// the session is accepted.  This allows tests to reject or customize the response.
pub type RequestHandler = Box<
    dyn FnMut(web_transport_quinn::Request) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + 'static,
>;

/// Start a WebTransport server that passes the raw [web_transport_quinn::Request]
/// to the handler instead of auto-accepting it.
pub async fn start_with_request_handler(
    cert: &TestCert,
    handler: RequestHandler,
) -> Result<TestServer> {
    let addr: SocketAddr = "[::1]:0".parse().unwrap();

    let server = web_transport_quinn::ServerBuilder::new()
        .with_addr(addr)
        .with_certificate(cert.chain.clone(), cert.key.clone_key())?;

    let actual_addr = server.local_addr()?;
    let url = format!("https://localhost:{}", actual_addr.port());

    tracing::debug!(%url, "test server listening (request handler)");

    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let handler_tasks: TaskHandles = Arc::new(Mutex::new(Vec::new()));
    let handler_tasks2 = handler_tasks.clone();

    let task = tokio::spawn(async move {
        let mut server = server;
        let mut handler = handler;
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                request = server.accept() => {
                    let Some(request) = request else { break };
                    let fut = handler(request);
                    let handle = tokio::spawn(fut);
                    handler_tasks2.lock().unwrap().push(handle);
                }
            }
        }
    });

    Ok(TestServer {
        addr: actual_addr,
        url,
        shutdown_tx: Some(shutdown_tx),
        task: Some(task),
        handler_tasks,
    })
}
