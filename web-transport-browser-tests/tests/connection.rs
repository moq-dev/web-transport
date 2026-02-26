use std::time::Duration;

use web_transport_browser_tests::harness;
use web_transport_browser_tests::server::{is_session_closed, RequestHandler, ServerHandler};

mod common;
use common::{init_tracing, TIMEOUT};

// ---------------------------------------------------------------------------
// Session Close
// ---------------------------------------------------------------------------

#[tokio::test]
async fn close_client_server_sees_closed() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let err = session.closed().await;
            assert!(is_session_closed(&err), "unexpected session error: {err}");
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        wt.close({ closeCode: 99, reason: "bye" });
        await wt.closed;
        return { success: true, message: "client closed session" };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn close_client_with_code_and_reason() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let err = session.closed().await;
            match err {
                web_transport_quinn::SessionError::WebTransportError(
                    web_transport_quinn::WebTransportError::Closed(code, reason),
                ) => {
                    assert_eq!(code, 99, "close code should round-trip");
                    assert_eq!(reason, "bye", "close reason should round-trip");
                }
                other => {
                    panic!("expected WebTransportError::Closed, got {other:?}");
                }
            }
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        wt.close({ closeCode: 99, reason: "bye" });
        await wt.closed;
        return { success: true, message: "closed with code 99" };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn close_server_client_sees_closed() {
    init_tracing();
    let harness = harness::setup(harness::immediate_close_handler(7, "server goodbye"))
        .await
        .unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        await wt.closed;
        return { success: true, message: "server closed session" };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn connection_closed_resolves_on_server_close() {
    init_tracing();
    let harness = harness::setup(harness::immediate_close_handler(7, "server goodbye"))
        .await
        .unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const info = await wt.closed;
        const ok = info.closeCode === 7 && info.reason === "server goodbye";
        return {
            success: ok,
            message: "closeCode=" + info.closeCode + " reason=" + JSON.stringify(info.reason)
        };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn close_server_while_streaming() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            // Accept a bidi stream to confirm the client started streaming
            session.accept_bi().await.expect("accept_bi failed");
            tokio::time::sleep(Duration::from_millis(200)).await;
            session.close(55, b"mid-stream");
            session.closed().await;
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        try {
            const stream = await wt.createBidirectionalStream();
            const writer = stream.writable.getWriter();

            // Start writing but don't close — server will close the session
            await writer.write(new TextEncoder().encode("streaming..."));

            // Wait for the session to close.
            await wt.closed;

            // Verify we can't write anymore
            try {
                await writer.write(new TextEncoder().encode("after close"));
                return { success: false, message: "write after close should fail" };
            } catch (e) {
                return { success: true, message: "write failed after server close: " + e };
            }
        } catch (e) {
            // Server close may race with stream setup/write — that's still a valid
            // demonstration that the server close disrupted the client.
            return { success: true, message: "server close interrupted streaming: " + e };
        }
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

// ---------------------------------------------------------------------------
// Edge Cases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn session_rejection_by_server() {
    init_tracing();

    let handler: RequestHandler = Box::new(|request| {
        Box::pin(async move {
            request
                .reject(http::StatusCode::NOT_FOUND)
                .await
                .expect("reject failed");
        })
    });

    let harness = harness::setup_with_request_handler(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = new WebTransport(SERVER_URL, {
            serverCertificateHashes: [{
                algorithm: "sha-256",
                value: CERT_HASH,
            }],
        });
        try {
            await wt.ready;
            throw new Error("ready should have rejected");
        } catch (e) {
            if (!(e instanceof WebTransportError) || e.source !== "session") throw e;
        }
        return { success: true, message: "session rejected" };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}
