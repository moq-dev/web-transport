use std::time::Duration;

use web_transport_browser_tests::harness;
use web_transport_browser_tests::server::ServerHandler;
use web_transport_quinn::{SessionError, WebTransportError};

mod common;
use common::{init_tracing, TIMEOUT};

/// Reproduces the lost-waker bug when multiple tasks call `accept_bi()`
/// concurrently on the same session.
///
/// The browser opens N bidi streams. On the server side, N independent tasks
/// each call `session.accept_bi()` concurrently. With the unfold-based
/// implementation, only one waker is stored, so all but one task hang forever.
#[tokio::test]
async fn concurrent_accept_bi_from_multiple_tasks() {
    init_tracing();

    const N: usize = 3;

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let mut tasks = tokio::task::JoinSet::new();

            // Spawn N independent tasks that each call accept_bi() concurrently.
            // This is the pattern that triggers the lost-waker bug: the unfold
            // stream only stores one waker, so N-1 tasks never get woken.
            for i in 0..N {
                let session = session.clone();
                tasks.spawn(async move {
                    let (mut send, mut recv) = session
                        .accept_bi()
                        .await
                        .unwrap_or_else(|e| panic!("task {i}: accept_bi failed: {e}"));
                    let data = recv
                        .read_to_end(1024)
                        .await
                        .unwrap_or_else(|e| panic!("task {i}: read_to_end failed: {e}"));
                    send.write_all(&data)
                        .await
                        .unwrap_or_else(|e| panic!("task {i}: write_all failed: {e}"));
                    send.finish()
                        .unwrap_or_else(|e| panic!("task {i}: finish failed: {e}"));
                });
            }

            // All N tasks must complete. With the bug, this times out because
            // N-1 tasks are stuck in accept_bi() with dead wakers.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            let mut completed = 0;
            while let Some(result) = tokio::time::timeout_at(deadline, tasks.join_next())
                .await
                .ok()
                .flatten()
            {
                if let Err(e) = result {
                    if e.is_panic() {
                        std::panic::resume_unwind(e.into_panic());
                    }
                }
                completed += 1;
            }
            assert_eq!(
                completed, N,
                "only {completed}/{N} accept_bi tasks completed (lost waker bug)"
            );

            // Keep the session alive until the client closes.
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
            &format!(
                r#"
        const wt = await connectWebTransport();
        const N = {N};
        const promises = [];

        for (let i = 0; i < N; i++) {{
            promises.push((async () => {{
                const stream = await wt.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                const reader = stream.readable.getReader();

                const msg = "bi-" + i;
                await writer.write(new TextEncoder().encode(msg));
                await writer.close();

                let received = "";
                while (true) {{
                    const {{ value, done }} = await reader.read();
                    if (done) break;
                    received += new TextDecoder().decode(value);
                }}
                return received;
            }})());
        }}

        const results = await Promise.all(promises);
        const expected = Array.from({{ length: N }}, (_, i) => "bi-" + i);
        results.sort();
        expected.sort();
        const ok = JSON.stringify(results) === JSON.stringify(expected);
        wt.close();
        return {{
            success: ok,
            message: "results: " + JSON.stringify(results)
        }};
    "#
            ),
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

/// Tests that `accept_bi()` and `accept_uni()` work correctly when called
/// concurrently from separate tasks on the same session. They share a single
/// `Mutex<SessionAccept>` but operate on independent internal state.
#[tokio::test]
async fn concurrent_accept_bi_and_uni_from_multiple_tasks() {
    init_tracing();

    const N: usize = 3;

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let mut tasks = tokio::task::JoinSet::new();

            // Spawn N tasks for accept_bi and N tasks for accept_uni concurrently.
            for i in 0..N {
                let session = session.clone();
                tasks.spawn(async move {
                    let (mut send, mut recv) = session
                        .accept_bi()
                        .await
                        .unwrap_or_else(|e| panic!("bi task {i}: accept_bi failed: {e}"));
                    let data = recv
                        .read_to_end(1024)
                        .await
                        .unwrap_or_else(|e| panic!("bi task {i}: read_to_end failed: {e}"));
                    send.write_all(&data)
                        .await
                        .unwrap_or_else(|e| panic!("bi task {i}: write_all failed: {e}"));
                    send.finish()
                        .unwrap_or_else(|e| panic!("bi task {i}: finish failed: {e}"));
                    format!("bi-done-{i}")
                });
            }
            for i in 0..N {
                let session = session.clone();
                tasks.spawn(async move {
                    let mut recv = session
                        .accept_uni()
                        .await
                        .unwrap_or_else(|e| panic!("uni task {i}: accept_uni failed: {e}"));
                    let data = recv
                        .read_to_end(1024)
                        .await
                        .unwrap_or_else(|e| panic!("uni task {i}: read_to_end failed: {e}"));
                    String::from_utf8(data)
                        .unwrap_or_else(|e| panic!("uni task {i}: invalid utf8: {e}"))
                });
            }

            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            let mut completed = 0;
            while let Some(result) = tokio::time::timeout_at(deadline, tasks.join_next())
                .await
                .ok()
                .flatten()
            {
                if let Err(e) = result {
                    if e.is_panic() {
                        std::panic::resume_unwind(e.into_panic());
                    }
                }
                completed += 1;
            }
            assert_eq!(
                completed,
                N * 2,
                "only {completed}/{} accept tasks completed",
                N * 2
            );

            // Signal the browser that all streams were received.
            let mut signal = session
                .open_uni()
                .await
                .expect("open_uni for signal failed");
            signal.write_all(b"ok").await.expect("signal write failed");
            signal.finish().expect("signal finish failed");

            // Keep the session alive until the client closes.
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
            &format!(
                r#"
        const wt = await connectWebTransport();
        const N = {N};
        const promises = [];

        // Open N bidi streams (server echoes them back)
        for (let i = 0; i < N; i++) {{
            promises.push((async () => {{
                const stream = await wt.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                const reader = stream.readable.getReader();

                const msg = "bi-" + i;
                await writer.write(new TextEncoder().encode(msg));
                await writer.close();

                let received = "";
                while (true) {{
                    const {{ value, done }} = await reader.read();
                    if (done) break;
                    received += new TextDecoder().decode(value);
                }}
                return received;
            }})());
        }}

        // Open N uni streams
        for (let i = 0; i < N; i++) {{
            promises.push((async () => {{
                const stream = await wt.createUnidirectionalStream();
                const writer = stream.getWriter();
                await writer.write(new TextEncoder().encode("uni-" + i));
                await writer.close();
                return "uni-sent-" + i;
            }})());
        }}

        const results = await Promise.all(promises);

        // Wait for the server signal before closing.
        const reader = wt.incomingUnidirectionalStreams.getReader();
        const {{ value: signal }} = await reader.read();
        const sr = signal.getReader();
        await sr.read();

        wt.close();

        const biResults = results.slice(0, N).sort();
        const expected = Array.from({{ length: N }}, (_, i) => "bi-" + i).sort();
        const ok = JSON.stringify(biResults) === JSON.stringify(expected);
        return {{
            success: ok,
            message: "bi results: " + JSON.stringify(biResults)
        }};
    "#
            ),
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}

/// Same bug but for `accept_uni()`: multiple tasks calling it concurrently
/// causes lost wakers.
#[tokio::test]
async fn concurrent_accept_uni_from_multiple_tasks() {
    init_tracing();

    const N: usize = 3;

    let handler: ServerHandler = Box::new(|session| {
        Box::pin(async move {
            let mut tasks = tokio::task::JoinSet::new();

            for i in 0..N {
                let session = session.clone();
                tasks.spawn(async move {
                    let mut recv = session
                        .accept_uni()
                        .await
                        .unwrap_or_else(|e| panic!("task {i}: accept_uni failed: {e}"));
                    let data = recv
                        .read_to_end(1024)
                        .await
                        .unwrap_or_else(|e| panic!("task {i}: read_to_end failed: {e}"));
                    String::from_utf8(data)
                        .unwrap_or_else(|e| panic!("task {i}: invalid utf8: {e}"))
                });
            }

            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            let mut received = Vec::new();
            while let Some(result) = tokio::time::timeout_at(deadline, tasks.join_next())
                .await
                .ok()
                .flatten()
            {
                match result {
                    Ok(s) => received.push(s),
                    Err(e) => {
                        if e.is_panic() {
                            std::panic::resume_unwind(e.into_panic());
                        }
                    }
                }
            }
            received.sort();
            assert_eq!(
                received.len(),
                N,
                "only {}/{N} accept_uni tasks completed (lost waker bug); got: {received:?}",
                received.len()
            );

            // Signal the browser that all streams were received.
            let mut signal = session
                .open_uni()
                .await
                .expect("open_uni for signal failed");
            signal.write_all(b"ok").await.expect("signal write failed");
            signal.finish().expect("signal finish failed");

            // Keep the session alive until the client closes.
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
            &format!(
                r#"
        const wt = await connectWebTransport();
        const N = {N};
        const promises = [];

        for (let i = 0; i < N; i++) {{
            promises.push((async () => {{
                const stream = await wt.createUnidirectionalStream();
                const writer = stream.getWriter();
                await writer.write(new TextEncoder().encode("uni-" + i));
                await writer.close();
                return true;
            }})());
        }}

        await Promise.all(promises);

        // Wait for the server to signal that all streams were received
        // before closing, so we don't abort in-flight stream decodes.
        const reader = wt.incomingUnidirectionalStreams.getReader();
        const {{ value: signal }} = await reader.read();
        const sr = signal.getReader();
        await sr.read();

        wt.close();
        return {{ success: true, message: "sent " + N + " uni streams" }};
    "#
            ),
            TIMEOUT,
        )
        .await;

    harness.teardown().await;
    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}
