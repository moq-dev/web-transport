use web_transport_browser_tests::harness;
use web_transport_browser_tests::server::{is_session_closed, ServerHandler};

mod common;
use common::{init_tracing, LONG_TIMEOUT, TIMEOUT};

// ---------------------------------------------------------------------------
// Client-Initiated
// ---------------------------------------------------------------------------

#[tokio::test]
async fn uni_stream_client_to_server() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let mut recv = session.accept_uni().await.expect("accept_uni failed");
            let data = recv
                .read_to_end(1024 * 1024)
                .await
                .expect("read_to_end failed");
            assert_eq!(
                String::from_utf8_lossy(&data),
                "uni data",
                "server should receive 'uni data'"
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
        const stream = await wt.createUnidirectionalStream();
        const writer = stream.getWriter();
        await writer.write(new TextEncoder().encode("uni data"));
        await writer.close();
        wt.close();
        return { success: true, message: "uni stream sent" };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn uni_stream_client_to_server_multiple() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let mut collected = Vec::new();
            for _ in 0..3 {
                let mut recv = session.accept_uni().await.expect("accept_uni failed");
                let data = recv.read_to_end(1024).await.expect("read_to_end failed");
                collected.push(String::from_utf8_lossy(&data).into_owned());
            }
            assert_eq!(collected.len(), 3, "server should receive 3 messages");
            collected.sort();
            assert_eq!(collected, vec!["msg0", "msg1", "msg2"]);
            let err = session.closed().await;
            assert!(is_session_closed(&err), "unexpected session error: {err}");
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        for (let i = 0; i < 3; i++) {
            const stream = await wt.createUnidirectionalStream();
            const writer = stream.getWriter();
            await writer.write(new TextEncoder().encode("msg" + i));
            await writer.close();
        }
        wt.close();
        return { success: true, message: "3 uni streams sent" };
    "#,
            LONG_TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn uni_stream_client_large_payload() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let mut recv = session.accept_uni().await.expect("accept_uni failed");
            let data = recv
                .read_to_end(2 * 1024 * 1024)
                .await
                .expect("read_to_end failed");
            assert_eq!(data.len(), 1024 * 1024, "server should receive 1MB");
            // Spot-check pattern every 4KB: each byte = (index % 251) as u8
            for i in (0..data.len()).step_by(4096) {
                assert_eq!(data[i], (i % 251) as u8, "data mismatch at byte {i}");
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
        const stream = await wt.createUnidirectionalStream();
        const writer = stream.getWriter();

        const SIZE = 1024 * 1024;
        const data = new Uint8Array(SIZE);
        for (let i = 0; i < SIZE; i++) data[i] = i % 251;

        const CHUNK = 64 * 1024;
        for (let off = 0; off < SIZE; off += CHUNK) {
            await writer.write(data.subarray(off, Math.min(off + CHUNK, SIZE)));
        }
        await writer.close();
        wt.close();
        return { success: true, message: "sent 1MB via uni stream" };
    "#,
            LONG_TIMEOUT,
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
async fn uni_stream_server_to_client() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let mut send = session.open_uni().await.expect("open_uni failed");
            send.write_all(b"server uni")
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
        const reader = wt.incomingUnidirectionalStreams.getReader();
        const { value: stream } = await reader.read();

        const sr = stream.getReader();
        let received = "";
        while (true) {
            const { value, done } = await sr.read();
            if (done) break;
            received += new TextDecoder().decode(value);
        }
        wt.close();
        return {
            success: received === "server uni",
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
async fn uni_stream_server_to_client_multiple() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            for i in 0u8..3 {
                let mut send = session.open_uni().await.expect("open_uni failed");
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
        const reader = wt.incomingUnidirectionalStreams.getReader();
        const values = [];
        for (let i = 0; i < 3; i++) {
            const { value: stream, done } = await reader.read();
            if (done) break;
            const sr = stream.getReader();
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
async fn uni_stream_server_large_payload() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let mut send = session.open_uni().await.expect("open_uni failed");
            // 64KB of pattern data: each byte = (index % 251) as u8
            let data: Vec<u8> = (0..65536).map(|i| (i % 251) as u8).collect();
            send.write_all(&data).await.expect("write_all failed");
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
        const reader = wt.incomingUnidirectionalStreams.getReader();
        const { value: stream } = await reader.read();
        const sr = stream.getReader();

        const chunks = [];
        while (true) {
            const { value, done } = await sr.read();
            if (done) break;
            chunks.push(value);
        }
        const received = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
        let off = 0;
        for (const c of chunks) { received.set(c, off); off += c.length; }

        let ok = received.length === 65536;
        for (let i = 0; ok && i < received.length; i++) {
            if (received[i] !== (i % 251)) { ok = false; }
        }
        wt.close();
        return { success: ok, message: "len=" + received.length };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}
