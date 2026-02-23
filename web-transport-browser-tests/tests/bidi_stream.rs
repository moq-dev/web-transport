use web_transport_browser_tests::harness;
use web_transport_browser_tests::server::{is_session_closed, ServerHandler};

mod common;
use common::{init_tracing, TIMEOUT};

// ---------------------------------------------------------------------------
// Client-Initiated
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bidi_stream_echo() {
    init_tracing();
    let harness = harness::setup(harness::echo_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const stream = await wt.createBidirectionalStream();
        const writer = stream.writable.getWriter();
        const reader = stream.readable.getReader();

        await writer.write(new TextEncoder().encode("hello world"));
        await writer.close();

        let received = "";
        while (true) {
            const { value, done } = await reader.read();
            if (done) break;
            received += new TextDecoder().decode(value);
        }
        wt.close();
        return {
            success: received === "hello world",
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
async fn bidi_stream_binary_data() {
    init_tracing();
    let harness = harness::setup(harness::echo_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const stream = await wt.createBidirectionalStream();
        const writer = stream.writable.getWriter();
        const reader = stream.readable.getReader();

        // Build 256-byte array with values 0x00-0xFF
        const sent = new Uint8Array(256);
        for (let i = 0; i < 256; i++) sent[i] = i;
        await writer.write(sent);
        await writer.close();

        const chunks = [];
        while (true) {
            const { value, done } = await reader.read();
            if (done) break;
            chunks.push(value);
        }
        const received = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
        let off = 0;
        for (const c of chunks) { received.set(c, off); off += c.length; }

        let ok = received.length === 256;
        for (let i = 0; ok && i < 256; i++) ok = received[i] === i;

        wt.close();
        return { success: ok, message: "binary len=" + received.length };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn bidi_stream_empty_write() {
    init_tracing();
    let harness = harness::setup(harness::echo_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const stream = await wt.createBidirectionalStream();
        const writer = stream.writable.getWriter();
        const reader = stream.readable.getReader();

        // Immediately close without writing
        await writer.close();

        const { value, done } = await reader.read();
        wt.close();
        return {
            success: done === true && !value,
            message: "done=" + done + " value=" + value
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
async fn bidi_stream_multiple_writes() {
    init_tracing();
    let harness = harness::setup(harness::echo_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const stream = await wt.createBidirectionalStream();
        const writer = stream.writable.getWriter();
        const reader = stream.readable.getReader();

        const enc = new TextEncoder();
        await writer.write(enc.encode("aaa"));
        await writer.write(enc.encode("bbb"));
        await writer.write(enc.encode("ccc"));
        await writer.close();

        let received = "";
        while (true) {
            const { value, done } = await reader.read();
            if (done) break;
            received += new TextDecoder().decode(value);
        }
        wt.close();
        return {
            success: received === "aaabbbccc",
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

// ---------------------------------------------------------------------------
// Server-Initiated
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bidi_stream_server_initiated() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let (mut send, _recv) = session.open_bi().await.expect("open_bi failed");
            send.write_all(b"from server")
                .await
                .expect("write_all failed");
            send.finish().expect("finish failed");
            let err = session.closed().await;
            assert!(is_session_closed(&err), "unexpected session error: {err}");
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const reader = wt.incomingBidirectionalStreams.getReader();
        const { value: stream, done } = await reader.read();
        if (done) return { success: false, message: "no incoming stream" };

        const sr = stream.readable.getReader();
        let received = "";
        while (true) {
            const { value, done } = await sr.read();
            if (done) break;
            received += new TextDecoder().decode(value);
        }
        wt.close();
        return {
            success: received === "from server",
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

#[tokio::test]
async fn bidi_stream_server_initiated_multiple() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            for i in 0u8..3 {
                let (mut send, _recv) = session.open_bi().await.expect("open_bi failed");
                send.write_all(&[i]).await.expect("write_all failed");
                send.finish().expect("finish failed");
            }
            let err = session.closed().await;
            assert!(is_session_closed(&err), "unexpected session error: {err}");
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const reader = wt.incomingBidirectionalStreams.getReader();
        const values = [];
        for (let i = 0; i < 3; i++) {
            const { value: stream, done } = await reader.read();
            if (done) break;
            const sr = stream.readable.getReader();
            const { value } = await sr.read();
            values.push(value[0]);
        }
        wt.close();
        values.sort((a, b) => a - b);
        const ok = values.length === 3 && values[0] === 0 && values[1] === 1 && values[2] === 2;
        return { success: ok, message: "values: " + JSON.stringify(values) };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn bidi_stream_server_initiated_bidirectional_exchange() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let (mut send, mut recv) = session.open_bi().await.expect("open_bi failed");
            send.write_all(b"ping").await.expect("write_all failed");
            send.finish().expect("finish failed");
            let data = recv.read_to_end(1024).await.expect("read_to_end failed");
            assert_eq!(
                String::from_utf8_lossy(&data),
                "pong",
                "server should receive pong"
            );
            let err = session.closed().await;
            assert!(is_session_closed(&err), "unexpected session error: {err}");
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const reader = wt.incomingBidirectionalStreams.getReader();
        const { value: stream } = await reader.read();

        const sr = stream.readable.getReader();
        let msg = "";
        while (true) {
            const { value, done } = await sr.read();
            if (done) break;
            msg += new TextDecoder().decode(value);
        }

        const sw = stream.writable.getWriter();
        await sw.write(new TextEncoder().encode("pong"));
        await sw.close();
        wt.close();
        return { success: msg === "ping", message: "received: " + msg };
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
async fn write_to_closed_transport() {
    init_tracing();
    let harness = harness::setup(harness::idle_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const stream = await wt.createBidirectionalStream();
        const writer = stream.writable.getWriter();

        wt.close();
        // Wait for close to take effect
        await wt.closed;

        try {
            await writer.write(new TextEncoder().encode("after close"));
            return { success: false, message: "write should have failed" };
        } catch (e) {
            if (!(e instanceof WebTransportError) || e.source !== "session") throw e;
            return { success: true, message: "write failed after close: " + e };
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
async fn read_from_cancelled_stream() {
    init_tracing();
    let harness = harness::setup(harness::idle_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const stream = await wt.createBidirectionalStream();
        const reader = stream.readable.getReader();

        await reader.cancel();

        const { value, done } = await reader.read();
        wt.close();
        return {
            success: done === true,
            message: "done=" + done + " value=" + value
        };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}
