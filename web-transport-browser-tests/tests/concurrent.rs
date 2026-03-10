use std::time::Duration;

use web_transport_browser_tests::harness;
use web_transport_browser_tests::server::ServerHandler;
use web_transport_quinn::{SessionError, WebTransportError};

mod common;
use common::{init_tracing, LONG_TIMEOUT, TIMEOUT};

// ---------------------------------------------------------------------------
// Multiple Concurrent Streams
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multiple_bidi_streams_concurrent() {
    init_tracing();
    let harness = harness::setup(harness::echo_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const N = 5;

        // Open N streams and write concurrently
        const promises = [];
        for (let i = 0; i < N; i++) {
            promises.push((async () => {
                const stream = await wt.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                const reader = stream.readable.getReader();

                const msg = "stream" + i;
                await writer.write(new TextEncoder().encode(msg));
                await writer.close();

                let received = "";
                while (true) {
                    const { value, done } = await reader.read();
                    if (done) break;
                    received += new TextDecoder().decode(value);
                }
                return received;
            })());
        }

        const results = await Promise.all(promises);
        const expected = Array.from({ length: N }, (_, i) => "stream" + i);
        results.sort();
        expected.sort();
        const ok = JSON.stringify(results) === JSON.stringify(expected);
        wt.close();
        return {
            success: ok,
            message: "results: " + JSON.stringify(results)
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
// Race conditions, rapid creation, mixed server streams
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rapid_stream_creation() {
    init_tracing();
    let harness = harness::setup(harness::echo_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const N = 50;
        const promises = [];
        for (let i = 0; i < N; i++) {
            promises.push((async () => {
                const stream = await wt.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                const reader = stream.readable.getReader();
                await writer.write(new Uint8Array([i % 256]));
                await writer.close();
                const { value, done } = await reader.read();
                if (done) return false;
                return value.length === 1 && value[0] === (i % 256);
            })());
        }
        const results = await Promise.all(promises);
        const allOk = results.every(r => r === true);
        wt.close();
        return {
            success: allOk,
            message: results.filter(r => !r).length + " of " + N + " failed"
        };
    "#,
            LONG_TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn server_close_while_client_creating_streams() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            // Accept the first stream
            let (_send, mut recv) = session.accept_bi().await.expect("accept_bi failed");
            recv.read_to_end(32).await.expect("initial read failed");
            recv.received_reset().await.expect("expected close");
            tokio::time::sleep(Duration::from_millis(50)).await;
            session.close(99, b"closing");
            session.closed().await;
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const N = 20;
        const promises = [];
        for (let i = 0; i < N; i++) {
            promises.push((async () => {
                try {
                    const stream = await wt.createBidirectionalStream();
                    const writer = stream.writable.getWriter();
                    await writer.write(new Uint8Array([i]));
                    await writer.close();
                    return true;
                } catch (e) {
                    return false;
                }
            })());
        }
        const results = await Promise.all(promises);
        const succeeded = results.filter(r => r).length;
        const failed = results.filter(r => !r).length;
        try { await wt.closed; } catch (e) {}
        return {
            success: succeeded >= 1,
            message: "succeeded=" + succeeded + " failed=" + failed
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
async fn server_opens_bidi_and_uni_simultaneously() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let (mut bi_send, _bi_recv) = session.open_bi().await.expect("open_bi failed");
            bi_send
                .write_all(b"bidi-data")
                .await
                .expect("write_all failed");
            bi_send.finish().expect("finish failed");

            let mut uni_send = session.open_uni().await.expect("open_uni failed");
            uni_send
                .write_all(b"uni-data")
                .await
                .expect("write_all failed");
            uni_send.finish().expect("finish failed");

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
        const [bidiResult, uniResult] = await Promise.all([
            (async () => {
                const reader = wt.incomingBidirectionalStreams.getReader();
                const { value: stream, done } = await reader.read();
                if (done) return "";
                const sr = stream.readable.getReader();
                let msg = "";
                while (true) {
                    const { value, done } = await sr.read();
                    if (done) break;
                    msg += new TextDecoder().decode(value);
                }
                return msg;
            })(),
            (async () => {
                const reader = wt.incomingUnidirectionalStreams.getReader();
                const { value: stream, done } = await reader.read();
                if (done) return "";
                const sr = stream.getReader();
                let msg = "";
                while (true) {
                    const { value, done } = await sr.read();
                    if (done) break;
                    msg += new TextDecoder().decode(value);
                }
                return msg;
            })()
        ]);

        wt.close();
        const ok = bidiResult === "bidi-data" && uniResult === "uni-data";
        return {
            success: ok,
            message: "bidi=" + bidiResult + " uni=" + uniResult
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
async fn multiple_streams_mixed_types() {
    init_tracing();

    // Handler that echoes bidi, verifies uni, and echoes datagrams
    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            loop {
                tokio::select! {
                    stream = session.accept_bi() => {
                        let Ok((mut send, mut recv)) = stream else { break };
                        tokio::spawn(async move {
                            let buf = recv.read_to_end(1024 * 1024).await.expect("echo: read_to_end failed");
                            send.write_all(&buf).await.expect("echo: write_all failed");
                            send.finish().expect("echo: finish failed");
                        });
                    }
                    stream = session.accept_uni() => {
                        let Ok(mut recv) = stream else { break };
                        let session = session.clone();
                        tokio::spawn(async move {
                            let data = recv.read_to_end(1024 * 1024).await.expect("read_to_end failed");
                            assert_eq!(
                                String::from_utf8_lossy(&data),
                                "uni",
                                "server should receive uni stream data"
                            );
                            // Acknowledge receipt back to the client
                            let mut ack = session.open_uni().await.expect("open_uni for ack failed");
                            ack.write_all(b"uni-ack").await.expect("write ack failed");
                            ack.finish().expect("finish ack failed");
                        });
                    }
                    datagram = session.read_datagram() => {
                        let Ok(data) = datagram else { break };
                        session.send_datagram(data).expect("echo: send_datagram failed");
                    }
                }
            }
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();

        // Run bidi, uni, and datagram concurrently
        const [bidiResult, uniResult, dgramResult] = await Promise.all([
            // Bidi echo
            (async () => {
                const stream = await wt.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                const reader = stream.readable.getReader();
                await writer.write(new TextEncoder().encode("bidi"));
                await writer.close();
                let r = "";
                while (true) {
                    const { value, done } = await reader.read();
                    if (done) break;
                    r += new TextDecoder().decode(value);
                }
                return r === "bidi";
            })(),
            // Uni write + verify server received it
            (async () => {
                const stream = await wt.createUnidirectionalStream();
                const writer = stream.getWriter();
                await writer.write(new TextEncoder().encode("uni"));
                await writer.close();
                // Read the server's acknowledgment uni stream
                const uniReader = wt.incomingUnidirectionalStreams.getReader();
                const { value: ackStream, done } = await uniReader.read();
                if (done) return false;
                const sr = ackStream.getReader();
                let ack = "";
                while (true) {
                    const { value, done } = await sr.read();
                    if (done) break;
                    ack += new TextDecoder().decode(value);
                }
                return ack === "uni-ack";
            })(),
            // Datagram echo
            (async () => {
                const writer = wt.datagrams.writable.getWriter();
                const reader = wt.datagrams.readable.getReader();
                await writer.write(new TextEncoder().encode("dg"));
                const { value } = await reader.read();
                return new TextDecoder().decode(value) === "dg";
            })()
        ]);

        wt.close();
        return {
            success: bidiResult && uniResult && dgramResult,
            message: "bidi=" + bidiResult + " uni=" + uniResult + " dgram=" + dgramResult
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
async fn many_streams_stress() {
    init_tracing();
    let harness = harness::setup(harness::echo_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const N = 20;
        const promises = [];

        for (let i = 0; i < N; i++) {
            promises.push((async () => {
                const stream = await wt.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                const reader = stream.readable.getReader();

                const payload = new Uint8Array(64).fill(i);
                await writer.write(payload);
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

                if (received.length !== 64) return false;
                for (let j = 0; j < 64; j++) {
                    if (received[j] !== i) return false;
                }
                return true;
            })());
        }

        const results = await Promise.all(promises);
        const allOk = results.every(r => r === true);
        wt.close();
        return {
            success: allOk,
            message: results.filter(r => !r).length + " of " + N + " failed"
        };
    "#,
            LONG_TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

// ---------------------------------------------------------------------------
// Large Data Transfer
// ---------------------------------------------------------------------------

#[tokio::test]
async fn large_bidi_stream_256kb() {
    init_tracing();
    let harness = harness::setup(harness::echo_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const stream = await wt.createBidirectionalStream();
        const writer = stream.writable.getWriter();
        const reader = stream.readable.getReader();

        const SIZE = 256 * 1024;
        const sent = new Uint8Array(SIZE);
        for (let i = 0; i < SIZE; i++) sent[i] = i % 251;
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

        let ok = received.length === SIZE;
        for (let i = 0; ok && i < SIZE; i += 1024) {
            if (received[i] !== (i % 251)) ok = false;
        }
        wt.close();
        return { success: ok, message: "len=" + received.length };
    "#,
            LONG_TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn large_bidi_stream_1mb() {
    init_tracing();
    let harness = harness::setup(harness::echo_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const stream = await wt.createBidirectionalStream();
        const writer = stream.writable.getWriter();
        const reader = stream.readable.getReader();

        const SIZE = 1024 * 1024;
        const sent = new Uint8Array(SIZE);
        for (let i = 0; i < SIZE; i++) sent[i] = i % 251;

        // Write in chunks to avoid memory pressure
        const CHUNK = 64 * 1024;
        for (let off = 0; off < SIZE; off += CHUNK) {
            await writer.write(sent.subarray(off, Math.min(off + CHUNK, SIZE)));
        }
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

        let ok = received.length === SIZE;
        // Spot-check pattern every 4KB
        for (let i = 0; ok && i < SIZE; i += 4096) {
            if (received[i] !== (i % 251)) ok = false;
        }
        wt.close();
        return { success: ok, message: "len=" + received.length };
    "#,
            LONG_TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn large_uni_stream_client_to_server_1mb() {
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
            let _ = session.closed().await;
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
// Multiple Sessions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multiple_sessions_sequential() {
    init_tracing();
    let harness = harness::setup(harness::echo_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const N = 20;
        const echoes = [];
        for (let i = 0; i < N; i++) {
            const wt = await connectWebTransport();
            const s = await wt.createBidirectionalStream();
            const w = s.writable.getWriter();
            const r = s.readable.getReader();
            const msg = "session" + i;
            await w.write(new TextEncoder().encode(msg));
            await w.close();
            let echo = "";
            while (true) {
                const { value, done } = await r.read();
                if (done) break;
                echo += new TextDecoder().decode(value);
            }
            echoes.push(echo);
            wt.close();
            try { await wt.closed; } catch (e) {
                if (!(e instanceof WebTransportError) || e.source !== "session") throw e;
            }
        }
        const expected = Array.from({ length: N }, (_, i) => "session" + i);
        const ok = JSON.stringify(echoes) === JSON.stringify(expected);
        return { success: ok, message: "echoes: " + JSON.stringify(echoes) };
    "#,
            LONG_TIMEOUT,
        )
        .await;

    harness.teardown_expecting(20).await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn multiple_sessions_concurrent() {
    init_tracing();
    let harness = harness::setup(harness::echo_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const N = 20;
        const promises = [];
        for (let i = 0; i < N; i++) {
            promises.push((async () => {
                const wt = await connectWebTransport();
                const s = await wt.createBidirectionalStream();
                const w = s.writable.getWriter();
                const r = s.readable.getReader();
                const msg = "sess-" + i;
                await w.write(new TextEncoder().encode(msg));
                await w.close();
                let echo = "";
                while (true) {
                    const { value, done } = await r.read();
                    if (done) break;
                    echo += new TextDecoder().decode(value);
                }
                wt.close();
                return { i, echo };
            })());
        }
        const results = await Promise.all(promises);
        const failed = results.filter(r => r.echo !== "sess-" + r.i);
        return {
            success: failed.length === 0,
            message: failed.length + " of " + N + " failed: " + JSON.stringify(failed)
        };
    "#,
            LONG_TIMEOUT,
        )
        .await;

    harness.teardown_expecting(20).await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

// ---------------------------------------------------------------------------
// Bidirectional Open
// ---------------------------------------------------------------------------

/// Both client and server open bidi streams concurrently. The server opens
/// N streams sending "s0"…"s{N-1}", while the client opens N streams sending
/// "c0"…"c{N-1}". Each side reads the peer's data and echoes it back.
#[tokio::test]
async fn bidirectional_open() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let mut tasks = tokio::task::JoinSet::new();
            let n = 5usize;

            // Server opens N bidi streams and sends data
            for i in 0..n {
                let session = session.clone();
                tasks.spawn(async move {
                    let (mut send, mut recv) = session.open_bi().await.expect("open_bi failed");
                    let msg = format!("s{i}");
                    send.write_all(msg.as_bytes())
                        .await
                        .expect("write_all failed");
                    send.finish().expect("finish failed");
                    let data = recv.read_to_end(1024).await.expect("read_to_end failed");
                    assert_eq!(
                        String::from_utf8_lossy(&data),
                        msg,
                        "server stream {i}: expected echo"
                    );
                });
            }

            // Server accepts N client-opened bidi streams and echoes them
            for _ in 0..n {
                match session.accept_bi().await {
                    Ok((mut send, mut recv)) => {
                        tasks.spawn(async move {
                            let data = recv.read_to_end(1024).await.expect("read_to_end failed");
                            send.write_all(&data).await.expect("write_all failed");
                            send.finish().expect("finish failed");
                        });
                    }
                    Err(e) => panic!("accept_bi failed: {e}"),
                }
            }

            // Wait for all tasks, propagate panics
            while let Some(result) = tasks.join_next().await {
                if let Err(e) = result {
                    if e.is_panic() {
                        std::panic::resume_unwind(e.into_panic());
                    }
                }
            }

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
        const N = 5;
        const errors = [];

        // Client opens N bidi streams and sends data
        const clientOpenPromises = [];
        for (let i = 0; i < N; i++) {
            clientOpenPromises.push((async () => {
                const stream = await wt.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                const reader = stream.readable.getReader();
                const msg = "c" + i;
                await writer.write(new TextEncoder().encode(msg));
                await writer.close();
                let received = "";
                while (true) {
                    const { value, done } = await reader.read();
                    if (done) break;
                    received += new TextDecoder().decode(value);
                }
                if (received !== msg) errors.push("client stream " + i + ": expected " + msg + " got " + received);
            })());
        }

        // Client accepts N server-opened bidi streams and echoes them
        const serverStreamReader = wt.incomingBidirectionalStreams.getReader();
        const clientAcceptPromises = [];
        for (let i = 0; i < N; i++) {
            clientAcceptPromises.push((async () => {
                const { value: stream, done } = await serverStreamReader.read();
                if (done) { errors.push("incoming bidi stream ended early"); return; }
                const reader = stream.readable.getReader();
                const writer = stream.writable.getWriter();
                let received = "";
                while (true) {
                    const { value, done } = await reader.read();
                    if (done) break;
                    received += new TextDecoder().decode(value);
                }
                // Echo back what the server sent
                await writer.write(new TextEncoder().encode(received));
                await writer.close();
            })());
        }

        await Promise.all([...clientOpenPromises, ...clientAcceptPromises]);
        wt.close();
        return {
            success: errors.length === 0,
            message: errors.length === 0 ? "all streams OK" : errors.join("; ")
        };
    "#,
            LONG_TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}
