use std::time::Duration;

use bytes::Bytes;
use web_transport_browser_tests::harness;
use web_transport_browser_tests::server::ServerHandler;
use web_transport_quinn::{SessionError, WebTransportError};

mod common;
use common::{init_tracing, TIMEOUT};

#[tokio::test]
async fn datagram_echo() {
    init_tracing();
    let harness = harness::setup(harness::echo_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const writer = wt.datagrams.writable.getWriter();
        const reader = wt.datagrams.readable.getReader();

        await writer.write(new TextEncoder().encode("dgram hello"));
        const { value } = await reader.read();
        const received = new TextDecoder().decode(value);
        wt.close();
        return {
            success: received === "dgram hello",
            message: "echoed: " + received
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
async fn datagram_binary_data() {
    init_tracing();
    let harness = harness::setup(harness::echo_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const writer = wt.datagrams.writable.getWriter();
        const reader = wt.datagrams.readable.getReader();

        const sent = new Uint8Array([0, 128, 255, 42]);
        await writer.write(sent);
        const { value } = await reader.read();

        const ok = value.length === 4 &&
            value[0] === 0 && value[1] === 128 &&
            value[2] === 255 && value[3] === 42;
        wt.close();
        return { success: ok, message: "received: [" + Array.from(value) + "]" };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn datagram_multiple_roundtrips() {
    init_tracing();
    let harness = harness::setup(harness::echo_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const writer = wt.datagrams.writable.getWriter();
        const reader = wt.datagrams.readable.getReader();

        // Send 10 datagrams
        for (let i = 0; i < 10; i++) {
            await writer.write(new Uint8Array([i]));
        }

        // Wait for a single echoed datagram
        const { value } = await reader.read();
        const valid = value.length === 1 && value[0] >= 0 && value[0] <= 9;
        reader.releaseLock();
        wt.close();
        return {
            success: valid,
            message: "received datagram with value " + Array.from(value)
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
async fn datagram_max_size() {
    init_tracing();
    let harness = harness::setup(harness::idle_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const maxSize = wt.datagrams.maxDatagramSize;
        wt.close();
        return {
            success: typeof maxSize === "number" && maxSize >= 500,
            message: "maxDatagramSize=" + maxSize
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
async fn datagram_server_initiated() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            // Small delay to let the client set up a datagram reader
            tokio::time::sleep(Duration::from_millis(100)).await;
            session
                .send_datagram(Bytes::from_static(b"server dgram"))
                .expect("send_datagram failed");
            let err = session.closed().await;
            assert!(
                matches!(err, SessionError::WebTransportError(WebTransportError::Closed(_, _))),
                "expected WebTransportError::Closed, got {err}"
            );
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const reader = wt.datagrams.readable.getReader();
        const { value } = await reader.read();
        const received = new TextDecoder().decode(value);
        wt.close();
        return {
            success: received === "server dgram",
            message: "received: " + received
        };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}
