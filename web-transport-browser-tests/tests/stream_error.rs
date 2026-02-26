use std::time::Duration;

use web_transport_browser_tests::harness;
use web_transport_browser_tests::server::ServerHandler;
use web_transport_quinn::{ReadError, SessionError, WebTransportError, WriteError};

mod common;
use common::{init_tracing, TIMEOUT};

#[tokio::test]
async fn stream_client_abort_sends_reset() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let (_send, mut recv) = session.accept_bi().await.expect("accept_bi failed");
            let code = recv.received_reset().await.ok().flatten();
            assert_eq!(
                code,
                Some(42),
                "server should receive RESET_STREAM with code 42"
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

        // Construct WebTransportError — try both (message, init) and (init) forms
        let err = new WebTransportError({ message: "abort", streamErrorCode: 42 });
        await writer.abort(err);
        wt.close();
        return { success: true, message: "writer aborted with code 42" };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn stream_client_cancel_sends_stop_sending() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let (send, _recv) = session.accept_bi().await.expect("accept_bi failed");
            let code = send.stopped().await.ok().flatten();
            assert_eq!(
                code,
                Some(77),
                "server should receive STOP_SENDING with code 77"
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
        const reader = stream.readable.getReader();

        // Construct WebTransportError — try both (message, init) and (init) forms
        let err = new WebTransportError({ message: "cancel", streamErrorCode: 77 });
        await reader.cancel(err);
        wt.close();
        return { success: true, message: "reader cancelled with code 77" };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn stream_client_reset_server_reader_errors() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let (_send, mut recv) = session.accept_bi().await.expect("accept_bi failed");
            let mut buf = [0u8; 1024];
            loop {
                match recv.read(&mut buf).await {
                    Ok(Some(_)) => continue,
                    Ok(None) => panic!("expected reset, got clean finish"),
                    Err(ReadError::Reset(code)) => {
                        assert_eq!(code, 42, "reset code should be 42");
                        break;
                    }
                    Err(e) => panic!("unexpected read error: {e}"),
                }
            }
            session.close(0, b"");
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const stream = await wt.createBidirectionalStream();
        const writer = stream.writable.getWriter();

        await writer.write(new TextEncoder().encode("some data"));
        let err = new WebTransportError({ message: "abort", streamErrorCode: 42 });
        await writer.abort(err);
        // Wait for the server to observe the reset and close the session
        try {
            await wt.closed;
            throw new Error("wt.closed should have rejected");
        } catch (e) {
            if (!(e instanceof WebTransportError) || e.source !== "session") throw e;
        }
        return { success: true, message: "writer aborted with code 42" };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn stream_client_stop_server_writer_errors() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let (mut send, _recv) = session.accept_bi().await.expect("accept_bi failed");
            // Keep writing until we get a Stopped error from the client's cancel
            let chunk = vec![0u8; 1024];
            loop {
                match send.write_all(&chunk).await {
                    Ok(()) => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(WriteError::Stopped(code)) => {
                        assert_eq!(code, 77, "stop code should be 77");
                        break;
                    }
                    Err(e) => panic!("unexpected write error: {e}"),
                }
            }
            session.close(0, b"");
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

        // Write to trigger server accept
        await writer.write(new Uint8Array([1]));
        // Read to confirm the server has started writing
        await reader.read();

        let err = new WebTransportError({ message: "cancel", streamErrorCode: 77 });
        await reader.cancel(err);
        // Wait for the server to observe the stop and close the session
        try {
            await wt.closed;
            throw new Error("wt.closed should have rejected");
        } catch (e) {
            if (!(e instanceof WebTransportError) || e.source !== "session") throw e;
        }
        return { success: true, message: "reader cancelled with code 77" };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn stream_server_reset_client_reader_errors() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let (mut send, mut recv) = session.accept_bi().await.expect("accept_bi failed");
            // Read to wait until the client starts
            let mut buf = [0u8; 1024];
            recv.read(&mut buf).await.expect("read failed");
            send.reset(33).expect("reset failed");
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
        try {
            const stream = await wt.createBidirectionalStream();
            const reader = stream.readable.getReader();
            const writer = stream.writable.getWriter();
            await writer.write(new Uint8Array([1]));

            while (true) {
                const { done } = await reader.read();
                if (done) {
                    wt.close();
                    return { success: false, message: "expected reader to error on reset" };
                }
            }
        } catch (e) {
            const isWTE = e instanceof WebTransportError;
            const code = isWTE ? e.streamErrorCode : null;
            wt.close();
            return {
                success: isWTE && e.source === "stream" && code === 33,
                message: "reader errored: isWebTransportError=" + isWTE + " code=" + code + " " + e,
                details: { streamErrorCode: code }
            };
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
async fn stream_server_stop_client_writer_errors() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let (_send, mut recv) = session.accept_bi().await.expect("accept_bi failed");
            // Read so QUIC acknowledges the stream before calling stop()
            let mut buf = [0u8; 1024];
            recv.read(&mut buf).await.expect("read failed");
            recv.stop(88).expect("stop failed");
            // small delay to make sure stop propagates
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
        try {
            const stream = await wt.createBidirectionalStream();
            const writer = stream.writable.getWriter();
            await writer.write(new Uint8Array([1]));

            // Keep writing until we get an error from STOP_SENDING
            for (let i = 0; i < 100; i++) {
                await writer.write(new Uint8Array(1024));
                await new Promise(r => setTimeout(r, 10));
            }
            wt.close();
            return { success: false, message: "expected writer to error on stop" };
        } catch (e) {
            const isWTE = e instanceof WebTransportError;
            const code = isWTE ? e.streamErrorCode : null;
            wt.close();
            return {
                success: isWTE && e.source === "stream" && code === 88,
                message: "writer errored: isWebTransportError=" + isWTE + " code=" + code + " " + e,
                details: { streamErrorCode: code }
            };
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
// Connection close interrupts stream I/O
// ---------------------------------------------------------------------------

#[tokio::test]
async fn client_close_interrupts_server_read() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let (_send, mut recv) = session.accept_bi().await.expect("accept_bi failed");
            let mut buf = [0u8; 1024];
            loop {
                match recv.read(&mut buf).await {
                    Ok(Some(_)) => continue,
                    Ok(None) => panic!("expected connection error, got clean finish"),
                    Err(ReadError::SessionError(SessionError::WebTransportError(
                        WebTransportError::Closed(_, _),
                    ))) => break,
                    Err(e) => panic!("expected session closed, got {e}"),
                }
            }
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const stream = await wt.createBidirectionalStream();
        const writer = stream.writable.getWriter();
        await writer.write(new TextEncoder().encode("some data"));
        wt.close();
        await wt.closed;
        return { success: true, message: "client closed while server was reading" };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn client_close_interrupts_server_write() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let (mut send, mut recv) = session.accept_bi().await.expect("accept_bi failed");
            // Read to confirm the client has opened the stream
            let mut buf = [0u8; 1024];
            recv.read(&mut buf).await.expect("read failed");
            let chunk = vec![0u8; 1024];
            loop {
                match send.write_all(&chunk).await {
                    Ok(()) => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(WriteError::SessionError(SessionError::WebTransportError(
                        WebTransportError::Closed(_, _),
                    ))) => break,
                    Err(e) => panic!("expected session closed, got {e}"),
                }
            }
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const stream = await wt.createBidirectionalStream();
        const writer = stream.writable.getWriter();
        // Write to trigger server accept
        await writer.write(new Uint8Array([1]));
        // Small delay so the server starts writing
        await new Promise(r => setTimeout(r, 100));
        wt.close();
        await wt.closed;
        return { success: true, message: "client closed while server was writing" };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn server_close_interrupts_client_read() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let (_send, mut recv) = session.accept_bi().await.expect("accept_bi failed");
            // Read to confirm the client has started
            let mut buf = [0u8; 1024];
            recv.read(&mut buf).await.expect("read failed");
            session.close(0, b"");
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
        // Write to trigger server accept
        await writer.write(new Uint8Array([1]));
        try {
            while (true) {
                const { done } = await reader.read();
                if (done) {
                    return { success: false, message: "expected error, got clean finish" };
                }
            }
        } catch (e) {
            if (!(e instanceof WebTransportError) || e.source !== "session") throw e;
            return { success: true, message: "read interrupted by server close: " + e };
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
async fn server_close_interrupts_client_write() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let (_send, mut recv) = session.accept_bi().await.expect("accept_bi failed");
            // Read to confirm the client has started
            let mut buf = [0u8; 1024];
            recv.read(&mut buf).await.expect("read failed");
            session.close(0, b"");
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const stream = await wt.createBidirectionalStream();
        const writer = stream.writable.getWriter();
        // Write to trigger server accept
        await writer.write(new Uint8Array([1]));
        try {
            for (let i = 0; i < 100; i++) {
                await writer.write(new Uint8Array(1024));
                await new Promise(r => setTimeout(r, 10));
            }
            return { success: false, message: "expected error on write" };
        } catch (e) {
            if (!(e instanceof WebTransportError) || e.source !== "session") throw e;
            return { success: true, message: "write interrupted by server close: " + e };
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
// Connection close interrupts accept
// ---------------------------------------------------------------------------

#[tokio::test]
async fn client_close_interrupts_server_accept_bi() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            // First accept succeeds — client opened one stream before closing
            // Keep references to streams to avoid early cancellation
            let _s1 = session.accept_bi().await.expect("first accept_bi failed");
            // Second accept should fail with a session close error
            let err = session.accept_bi().await.unwrap_err();
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
        await writer.write(new Uint8Array([1]));
        wt.close();
        await wt.closed;
        return { success: true, message: "client closed while server was accepting bidi" };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn client_close_interrupts_server_accept_uni() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            // First accept succeeds — client opened one stream before closing
            // Keep references to streams to avoid early cancellation
            let _s1 = session.accept_uni().await.expect("first accept_uni failed");
            // Second accept should fail with a session close error
            let err = session.accept_uni().await.unwrap_err();
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
        const stream = await wt.createUnidirectionalStream();
        const writer = stream.getWriter();
        await writer.write(new Uint8Array([1]));
        await writer.close();
        wt.close();
        await wt.closed;
        return { success: true, message: "client closed while server was accepting uni" };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

#[tokio::test]
async fn server_close_interrupts_client_accept_bi() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            // Small delay so the client starts waiting on incomingBidirectionalStreams
            tokio::time::sleep(Duration::from_millis(100)).await;
            session.close(0, b"");
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const reader = wt.incomingBidirectionalStreams.getReader();
        try {
            const { done } = await reader.read();
            if (done) {
                return { success: true, message: "incomingBidirectionalStreams closed" };
            }
            return { success: false, message: "expected stream to end, got a value" };
        } catch (e) {
            if (!(e instanceof WebTransportError) || e.source !== "session") throw e;
            return { success: true, message: "accept bidi interrupted by server close: " + e };
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
async fn server_close_interrupts_client_accept_uni() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            // Small delay so the client starts waiting on incomingUnidirectionalStreams
            tokio::time::sleep(Duration::from_millis(100)).await;
            session.close(0, b"");
        })
    });

    let harness = harness::setup(handler).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        const reader = wt.incomingUnidirectionalStreams.getReader();
        try {
            const { done } = await reader.read();
            if (done) {
                return { success: true, message: "incomingUnidirectionalStreams closed" };
            }
            return { success: false, message: "expected stream to end, got a value" };
        } catch (e) {
            if (!(e instanceof WebTransportError) || e.source !== "session") throw e;
            return { success: true, message: "accept uni interrupted by server close: " + e };
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
// Stream isolation
// ---------------------------------------------------------------------------

/// Opens 3 bidi streams on one session. Stream 0 is echoed normally, stream 1
/// is reset by the client, stream 2 is reset by the server. Verifies that the
/// resets on streams 1 and 2 do not disturb stream 0.
#[tokio::test]
async fn stream_reset_does_not_affect_other_streams() {
    init_tracing();

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                match session.accept_bi().await {
                    Ok((mut send, mut recv)) => {
                        tasks.spawn(async move {
                            match recv.read_to_end(1024 * 1024).await {
                                Ok(data) if data == b"reset-this" => {
                                    send.reset(33).expect("reset failed");
                                }
                                Ok(data) => {
                                    send.write_all(&data).await.expect("echo: write_all failed");
                                    send.finish().expect("echo: finish failed");
                                }
                                Err(web_transport_quinn::ReadToEndError::ReadError(
                                    ReadError::Reset(_),
                                )) => {
                                    // Client reset this stream — expected
                                }
                                Err(e) => panic!("unexpected read error: {e}"),
                            }
                        });
                    }
                    Err(SessionError::WebTransportError(WebTransportError::Closed(_, _))) => break,
                    Err(e) => panic!("accept_bi failed: {e}"),
                }
            }
            while let Some(result) = tasks.join_next().await {
                if let Err(e) = result {
                    if e.is_panic() {
                        std::panic::resume_unwind(e.into_panic());
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

        const [echoResult, clientResetResult, serverResetResult] = await Promise.allSettled([
            // Stream 0: normal echo
            (async () => {
                const stream = await wt.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                const reader = stream.readable.getReader();
                await writer.write(new TextEncoder().encode("stream0"));
                await writer.close();
                let received = "";
                while (true) {
                    const { value, done } = await reader.read();
                    if (done) break;
                    received += new TextDecoder().decode(value);
                }
                return received;
            })(),
            // Stream 1: client resets writer
            (async () => {
                const stream = await wt.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                await writer.write(new TextEncoder().encode("data"));
                const err = new WebTransportError({ message: "abort", streamErrorCode: 42 });
                await writer.abort(err);
            })(),
            // Stream 2: server resets after reading
            (async () => {
                const stream = await wt.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                const reader = stream.readable.getReader();
                await writer.write(new TextEncoder().encode("reset-this"));
                await writer.close();
                while (true) {
                    const { done } = await reader.read();
                    if (done) throw new Error("expected reset, got clean finish");
                }
            })()
        ]);

        wt.close();

        const echoOk = echoResult.status === "fulfilled" && echoResult.value === "stream0";
        const clientResetOk = clientResetResult.status === "fulfilled";
        const srErr = serverResetResult.reason;
        const serverResetOk = serverResetResult.status === "rejected"
            && srErr instanceof WebTransportError
            && srErr.source === "stream"
            && srErr.streamErrorCode === 33;

        return {
            success: echoOk && clientResetOk && serverResetOk,
            message: "echo=" + echoOk + " clientReset=" + clientResetOk
                + " serverReset=" + serverResetOk
                + " echoVal=" + JSON.stringify(echoResult.value)
                + " srSource=" + (srErr instanceof WebTransportError ? srErr.source : "N/A")
                + " srCode=" + (srErr instanceof WebTransportError ? srErr.streamErrorCode : "N/A")
        };
    "#,
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}
