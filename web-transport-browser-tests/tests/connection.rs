use std::time::Duration;

use bytes::Bytes;
use web_transport_browser_tests::harness;
use web_transport_browser_tests::server::{RequestHandler, ServerHandler};
use web_transport_quinn::generic::Stats;
use web_transport_quinn::{quinn, SessionError, WebTransportError};

mod common;
use common::{init_tracing, TIMEOUT};

// ---------------------------------------------------------------------------
// Session Stats
// ---------------------------------------------------------------------------

#[tokio::test]
async fn session_stats_after_data_transfer() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let (mut send, mut recv) = session.accept_bi().await.expect("accept_bi failed");
            let data = recv
                .read_to_end(64 * 1024)
                .await
                .expect("read_to_end failed");
            send.write_all(&data).await.expect("write_all failed");
            send.finish().expect("finish failed");

            let stats = session.stats();
            assert!(
                stats.bytes_sent().unwrap_or(0) > 0,
                "bytes_sent should be > 0"
            );
            assert!(
                stats.bytes_received().unwrap_or(0) > 0,
                "bytes_received should be > 0"
            );
            assert!(
                stats.packets_sent().unwrap_or(0) > 0,
                "packets_sent should be > 0"
            );
            assert!(
                stats.rtt().unwrap_or(Duration::ZERO) > Duration::ZERO,
                "rtt should be > 0"
            );

            let err = session.closed().await;
            assert!(
                matches!(
                    err,
                    SessionError::WebTransportError(WebTransportError::Closed(_, _))
                ),
                "expected WebTransportError::Closed, got {err}"
            );
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const stream = await wt.createBidirectionalStream();
        const writer = stream.writable.getWriter();
        const reader = stream.readable.getReader();

        const data = new Uint8Array(10 * 1024).fill(42);
        await writer.write(data);
        await writer.close();

        let total = 0;
        while (true) {
            const { value, done } = await reader.read();
            if (done) break;
            total += value.length;
        }
        wt.close();
        return { success: total === 10240, message: "echoed " + total + " bytes" };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

// ---------------------------------------------------------------------------
// Session Close
// ---------------------------------------------------------------------------

#[tokio::test]
async fn close_client_server_sees_closed() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let err = session.closed().await;
            assert!(
                matches!(
                    err,
                    SessionError::WebTransportError(WebTransportError::Closed(_, _))
                ),
                "expected WebTransportError::Closed, got {err}"
            );
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
            let (_send, mut recv) = session.accept_bi().await.expect("accept_bi failed");
            recv.read(&mut [1]).await.expect("read failed");
            session.close(55, b"mid-stream");
            let err = session.closed().await;
            assert!(
                matches!(
                    err,
                    SessionError::ConnectionError(quinn::ConnectionError::LocallyClosed)
                ),
                "expected ConnectionError::LocallyClosed, got {err}"
            );
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
            for (let i = 0; i < 10; i++) {
                const chunk = new Uint8Array(1024).fill(i);
                await writer.write(chunk);
                await new Promise(r => setTimeout(r, 50));
            }

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

#[tokio::test]
async fn close_client_while_streaming() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            // Accept a bidi stream to confirm the client started streaming
            let (mut send, _recv) = session.accept_bi().await.expect("accept_bi failed");

            // Start writing from the server side — the client will close mid-stream
            let chunk = vec![0xABu8; 4096];
            loop {
                match send.write_all(&chunk).await {
                    Ok(()) => {}
                    Err(_) => break,
                }
            }

            let err = session.closed().await;
            match err {
                SessionError::WebTransportError(WebTransportError::Closed(code, reason)) => {
                    assert_eq!(code, 77, "close code mismatch");
                    assert_eq!(reason, "client mid-stream", "close reason mismatch");
                }
                other => panic!(
                    "expected WebTransportError::Closed(77, \"client mid-stream\"), got {other:?}"
                ),
            }
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const stream = await wt.createBidirectionalStream();
        const reader = stream.readable.getReader();

        // Give the server a moment to start writing back
        await reader.read();

        // Close the session from the client while the server is writing
        wt.close({ closeCode: 77, reason: "client mid-stream" });
        await wt.closed;
        return { success: true, message: "client closed while server was streaming" };
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

// ---------------------------------------------------------------------------
// Close boundary values (parameterized)
// ---------------------------------------------------------------------------

macro_rules! close_code_reason_test {
    ($name:ident, $code:expr, $reason_js:expr, $reason_rust:expr) => {
        #[tokio::test]
        async fn $name() {
            init_tracing();

            let expected_code: u32 = $code;
            let expected_reason: String = ($reason_rust).to_string();

            let handler: ServerHandler = Box::new({
                let reason = expected_reason.clone();
                move |session| {
                    let reason = reason.clone();
                    Box::pin(async move {
                        let err = session.closed().await;
                        match err {
                            SessionError::WebTransportError(WebTransportError::Closed(c, r)) => {
                                assert_eq!(c, expected_code, "close code mismatch");
                                assert_eq!(r, reason, "close reason mismatch");
                            }
                            other => panic!("expected WebTransportError::Closed, got {other:?}"),
                        }
                    })
                }
            });

            let harness = harness::setup(handler).await.unwrap();

            let js_code = format!(
                r#"
                const wt = await connectWebTransport();
                wt.close({{ closeCode: {}, reason: {} }});
                await wt.closed;
                return {{ success: true, message: "closed" }};
                "#,
                expected_code, $reason_js
            );

            let result = harness.run_js(&js_code, TIMEOUT).await;
            harness.teardown().await;
            let result = result.unwrap();
            assert!(result.success, "{}", result.message);
        }
    };
}

close_code_reason_test!(close_code_zero, 0, r#""""#, "");
close_code_reason_test!(close_max_code, 4294967295u32, r#""max""#, "max");
close_code_reason_test!(close_empty_reason, 42, r#""""#, "");
close_code_reason_test!(close_unicode_reason, 1, r#""goodbye 👋🌍""#, "goodbye 👋🌍");
close_code_reason_test!(
    close_long_reason,
    1,
    r#""x".repeat(1024)"#,
    "x".repeat(1024)
);

// ---------------------------------------------------------------------------
// Server use-after-close (parameterized)
// ---------------------------------------------------------------------------

macro_rules! server_use_after_close_test {
    ($name:ident, |$session:ident| $op:expr) => {
        #[tokio::test]
        async fn $name() {
            init_tracing();

            let handler: ServerHandler = Box::new(|$session| {
                Box::pin(async move {
                    $session.close(7, b"done");
                    $session.closed().await;
                    let err = { $op }.unwrap_err();
                    assert!(
                        matches!(
                            err,
                            SessionError::ConnectionError(quinn::ConnectionError::LocallyClosed)
                        ),
                        "expected ConnectionError::LocallyClosed, got {err:?}"
                    );
                })
            });

            let harness = harness::setup(handler).await.unwrap();

            let result = harness
                .run_js(
                    r#"
                const wt = await connectWebTransport();
                try { await wt.closed; } catch (e) {
                    if (!(e instanceof WebTransportError)) throw e;
                }
                return { success: true, message: "session closed" };
                "#,
                    TIMEOUT,
                )
                .await;

            harness.teardown().await;
            let result = result.unwrap();
            assert!(result.success, "{}", result.message);
        }
    };
}

server_use_after_close_test!(server_open_bi_after_close, |session| session
    .open_bi()
    .await);
server_use_after_close_test!(server_open_uni_after_close, |session| session
    .open_uni()
    .await);
server_use_after_close_test!(server_accept_bi_after_close, |session| session
    .accept_bi()
    .await);
server_use_after_close_test!(server_accept_uni_after_close, |session| session
    .accept_uni()
    .await);

#[tokio::test]
async fn server_send_datagram_after_close() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            session.close(7, b"done");
            session.closed().await;
            let err = session
                .send_datagram(Bytes::from_static(b"test"))
                .unwrap_err();
            assert!(
                matches!(
                    err,
                    SessionError::ConnectionError(quinn::ConnectionError::LocallyClosed)
                ),
                "expected ConnectionError(LocallyClosed), got {err:?}"
            );
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
            const wt = await connectWebTransport();
            try { await wt.closed; } catch (e) {
                if (!(e instanceof WebTransportError)) throw e;
            }
            return { success: true, message: "session closed" };
            "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}
