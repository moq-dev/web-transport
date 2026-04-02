"""Browser interop tests for cross-boundary error propagation."""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING, Any

import pytest

import web_transport
from .conftest import _webtransport_connect_js

if TYPE_CHECKING:
    from .conftest import RunJS, RunJSRaw, ServerFactory

pytestmark = pytest.mark.asyncio(loop_scope="session")


async def test_server_close_during_browser_read(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server writes partial data, closes session → browser read sees error."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.open_bi()
                await send.write(b"partial")
                # Wait for browser to signal it accepted the stream
                await recv.read(1)
                # Close session abruptly — don't finish the stream
                session.close(1, "abort")

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                """
                try {
                    const reader = transport.incomingBidirectionalStreams.getReader();
                    const { value: stream } = await reader.read();
                    reader.releaseLock();
                    // Signal to server that we accepted the stream
                    const writer = stream.writable.getWriter();
                    await writer.write(new Uint8Array([1]));
                    writer.releaseLock();
                    const streamReader = stream.readable.getReader();
                    while (true) {
                        const { value, done } = await streamReader.read();
                        if (done) break;
                    }
                    return { errored: false };
                } catch (e) {
                    return { errored: true, message: e.toString() };
                }
            """,
            )

    assert isinstance(result, dict)
    assert result["errored"] is True


async def test_browser_close_during_server_read(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser writes partial, closes transport → server recv.read() raises SessionClosedByPeer."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                try:
                    send, recv = await session.accept_bi()
                    async with send:
                        # Try to read — browser will close mid-way
                        await recv.read()
                except (
                    web_transport.SessionClosedByPeer,
                    web_transport.SessionClosedLocally,
                    web_transport.StreamClosedByPeer,
                ) as e:
                    error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                await writer.write(new TextEncoder().encode("partial"));
                // Close transport abruptly (don't close the writer first)
                transport.close({closeCode: 1, reason: "abort"});
                return true;
            """,
            )

    assert error is not None


async def test_browser_close_during_server_write(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser closes while server writes large data → server write raises."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                try:
                    # Write a lot of data — browser will close mid-way
                    for _ in range(100):
                        await send.write(b"x" * 65536)
                except (
                    web_transport.SessionClosedByPeer,
                    web_transport.StreamClosedByPeer,
                    web_transport.SessionClosed,
                    web_transport.StreamClosed,
                ) as e:
                    error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const reader = stream.readable.getReader();
                // Wait for server to start writing by reading one chunk
                await reader.read();
                reader.releaseLock();
                transport.close({closeCode: 1, reason: "abort"});
                return true;
            """,
            )

    assert error is not None


async def test_open_bi_after_session_close_raises(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """session.close() then open_bi() raises SessionClosedLocally."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                session.close()
                await session.wait_closed()
                try:
                    await session.open_bi()
                except web_transport.SessionClosedLocally as e:
                    error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            try:
                await run_js(port, hash_b64, "await transport.closed; return true;")
            except Exception:
                pass

    assert isinstance(error, web_transport.SessionClosedLocally)


async def test_accept_bi_after_browser_close_raises(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser closes → pending accept_bi() raises SessionClosed."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            try:
                await session.accept_bi()
            except web_transport.SessionClosed as e:
                error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                transport.close({closeCode: 0, reason: ""});
                return true;
            """,
            )

    assert isinstance(error, web_transport.SessionClosed)


async def test_send_datagram_after_close_raises(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """session.close() then send_datagram() raises SessionClosed."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                session.close()
                await session.wait_closed()
                try:
                    session.send_datagram(b"too late")
                except web_transport.SessionClosed as e:
                    error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            try:
                await run_js(port, hash_b64, "await transport.closed; return true;")
            except Exception:
                pass

    assert isinstance(error, web_transport.SessionClosed)


async def test_receive_datagram_after_browser_close_raises(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser closes → pending receive_datagram() raises SessionClosed."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            try:
                await session.receive_datagram()
            except web_transport.SessionClosed as e:
                error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                transport.close({closeCode: 0, reason: ""});
                return true;
            """,
            )

    assert isinstance(error, web_transport.SessionClosed)


async def test_server_close_all_connections(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """server.close() causes browser's transport.closed to resolve."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            await request.accept()
            server.close()

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                await transport.ready;
                try {{
                    await transport.closed;
                    return {{ closed: true }};
                }} catch (e) {{
                    return {{ closed: true, error: e.toString() }};
                }}
            """)

    assert isinstance(result, dict)
    assert result["closed"] is True


# ---------------------------------------------------------------------------
# Stream state validation (server-side, browser-created stream)
# ---------------------------------------------------------------------------


async def test_double_reset_raises(start_server: ServerFactory, run_js: RunJS) -> None:
    """send.reset() twice → StreamClosedLocally."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                send.reset()
                try:
                    send.reset()
                except web_transport.StreamClosedLocally as e:
                    error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                try { await writer.close(); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert isinstance(error, web_transport.StreamClosedLocally)


async def test_double_stop_raises(start_server: ServerFactory, run_js: RunJS) -> None:
    """recv.stop() twice → StreamClosedLocally."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    recv.stop()
                    try:
                        recv.stop()
                    except web_transport.StreamClosedLocally as e:
                        error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                try {
                    const writer = stream.writable.getWriter();
                    await writer.close();
                } catch (e) {
                    // should raise because of stop()
                }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert isinstance(error, web_transport.StreamClosedLocally)


async def test_write_after_reset_raises(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """send.reset() then send.write() → StreamClosedLocally."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                send.reset()
                try:
                    await send.write(b"x")
                except web_transport.StreamClosedLocally as e:
                    error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                try { await writer.close(); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert isinstance(error, web_transport.StreamClosedLocally)


async def test_write_after_finish_raises(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """send.finish() then send.write() → StreamClosedLocally."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                await send.finish()
                try:
                    await send.write(b"x")
                except web_transport.StreamClosedLocally as e:
                    error = e
                await recv.read()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                // Read server's finished stream
                await readAll(stream.readable);
                const writer = stream.writable.getWriter();
                try { await writer.close(); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert isinstance(error, web_transport.StreamClosedLocally)


async def test_read_after_stop_raises(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """recv.stop() then recv.read() → StreamClosedLocally."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    recv.stop()
                    try:
                        await recv.read()
                    except web_transport.StreamClosedLocally as e:
                        error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                try {
                    const writer = stream.writable.getWriter();
                    await writer.close();
                } catch (e) {
                    // should raise because of stop()
                }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert isinstance(error, web_transport.StreamClosedLocally)


async def test_double_finish_raises(start_server: ServerFactory, run_js: RunJS) -> None:
    """send.finish() twice → StreamClosedLocally."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                await send.finish()
                try:
                    await send.finish()
                except web_transport.StreamClosedLocally as e:
                    error = e
                await recv.read()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                await readAll(stream.readable);
                const writer = stream.writable.getWriter();
                try { await writer.close(); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert isinstance(error, web_transport.StreamClosedLocally)


# ---------------------------------------------------------------------------
# Cross-boundary error propagation
# ---------------------------------------------------------------------------


async def test_browser_close_during_server_pending_read(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """Browser close interrupts server recv.read() → SessionClosed."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                await send.write(b"data")
                async with send:
                    try:
                        await recv.read()
                    except (
                        web_transport.SessionClosed,
                        web_transport.StreamClosedByPeer,
                    ) as e:
                        error = e

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                await transport.ready;
                const stream = await transport.createBidirectionalStream();
                const reader = stream.readable.getReader();
                await reader.read();
                // Don't write — just close the transport abruptly
                transport.close({{closeCode: 1, reason: "abort"}});
                await transport.closed;
                return true;
            """)

    assert error is not None


async def test_browser_close_during_server_pending_write(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """Browser close interrupts server send.write() (backpressured)."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                try:
                    # Write a lot — will block on backpressure
                    for _ in range(200):
                        await send.write(b"x" * 65536)
                except (
                    web_transport.SessionClosed,
                    web_transport.StreamClosed,
                ) as e:
                    error = e

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                await transport.ready;
                const stream = await transport.createBidirectionalStream();
                const reader = stream.readable.getReader();
                // Read one chunk to let server start writing
                await reader.read();
                reader.releaseLock();
                // Close transport while server is writing
                transport.close({{closeCode: 2, reason: "closing"}});
                await transport.closed;
                return true;
            """)

    assert error is not None


async def test_uni_send_reset(start_server: ServerFactory, run_js: RunJS) -> None:
    """Server opens uni stream, writes partial, resets → browser read errors."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send = await session.open_uni()
                await send.write(b"partial")
                # Wait for browser to signal it accepted the stream
                await session.receive_datagram()
                send.reset(7)
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                """
                const reader = transport.incomingUnidirectionalStreams.getReader();
                const { value: stream } = await reader.read();
                reader.releaseLock();
                // Signal to server that we accepted the stream
                const dgWriter = transport.datagrams.writable.getWriter();
                await dgWriter.write(new Uint8Array([1]));
                dgWriter.releaseLock();
                const streamReader = stream.getReader();
                try {
                    while (true) {
                        const { value, done } = await streamReader.read();
                        if (done) break;
                    }
                    return { errored: false };
                } catch (e) {
                    return { errored: true, message: e.toString() };
                }
            """,
            )

    assert isinstance(result, dict)
    assert result["errored"] is True


async def test_uni_recv_stop(start_server: ServerFactory, run_js: RunJS) -> None:
    """Browser opens uni stream → server recv.stop(7) → browser write errors."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                recv = await session.accept_uni()
                recv.stop(7)
                # Signal to browser that STOP_SENDING was sent
                session.send_datagram(b"\x01")
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createUnidirectionalStream();
                const writer = stream.getWriter();
                // Wait for server to signal STOP_SENDING was sent
                const dgReader = transport.datagrams.readable.getReader();
                await dgReader.read();
                dgReader.releaseLock();
                try {
                    // Write enough to trigger the error
                    for (let i = 0; i < 10; i++) {
                        await writer.write(new Uint8Array(65536));
                    }
                    return { errored: false };
                } catch (e) {
                    return { errored: true, message: e.toString() };
                }
            """,
            )

    assert isinstance(result, dict)
    assert result["errored"] is True


# ---------------------------------------------------------------------------
# wait_closed() with browser peer
# ---------------------------------------------------------------------------


async def test_send_wait_closed_returns_stop_code(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser cancels reader → send.wait_closed() returns error code."""
    async with start_server() as (server, port, hash_b64):
        stop_code: int | None = -1  # sentinel

        async def server_side() -> None:
            nonlocal stop_code
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                stop_code = await asyncio.wait_for(send.wait_closed(), timeout=5.0)
                await recv.read()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const reader = stream.readable.getReader();
                let err = new WebTransportError({ message: "stop", streamErrorCode: 42 });
                await reader.cancel(err);
                // Close writable so server can complete
                const writer = stream.writable.getWriter();
                try { await writer.close(); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert stop_code == 42


async def test_send_finish_then_wait_closed(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """finish() → browser reads → wait_closed() returns None."""
    async with start_server() as (server, port, hash_b64):
        result_code: int | None = -1  # sentinel

        async def server_side() -> None:
            nonlocal result_code
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                await send.write(b"data")
                await send.finish()
                result_code = await asyncio.wait_for(send.wait_closed(), timeout=5.0)
                await recv.read()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                // Read all data (acknowledges the stream)
                await readAll(stream.readable);
                // Close writable
                const writer = stream.writable.getWriter();
                try { await writer.close(); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert result_code is None


async def test_recv_wait_closed_returns_reset_code(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser aborts writer → recv.wait_closed() returns error code."""
    async with start_server() as (server, port, hash_b64):
        reset_code: int | None = -1  # sentinel

        async def server_side() -> None:
            nonlocal reset_code
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                await send.write(b"data")
                async with send:
                    reset_code = await asyncio.wait_for(recv.wait_closed(), timeout=5.0)
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                const reader = stream.readable.getReader();
                await reader.read();
                let err = new WebTransportError({ message: "abort", streamErrorCode: 99 });
                await writer.abort(err);
                return true;
            """,
            )

    assert reset_code == 99


async def test_recv_wait_closed_peer_finishes(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser closes writer → recv.wait_closed() returns None."""
    async with start_server() as (server, port, hash_b64):
        result_code: int | None = -1  # sentinel

        async def server_side() -> None:
            nonlocal result_code
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    # Read all data first
                    data = await recv.read()
                    assert data == b"hello"
                    result_code = await asyncio.wait_for(
                        recv.wait_closed(), timeout=5.0
                    )

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                try { await writeAllString(stream.writable, "hello"); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert result_code is None


# ---------------------------------------------------------------------------
# Server close interrupts client operations
# ---------------------------------------------------------------------------


async def test_server_close_interrupts_client_write(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server closes session while browser is writing to stream, browser write throws."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                # Read initial data to confirm stream is open
                await recv.read(1024)
                session.close(0, "")

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                await writer.write(new Uint8Array([1]));
                try {
                    for (let i = 0; i < 100; i++) {
                        await writer.write(new Uint8Array(1024));
                        await new Promise(r => setTimeout(r, 10));
                    }
                    return { interrupted: false };
                } catch (e) {
                    return { interrupted: true };
                }
            """,
            )

    assert isinstance(result, dict)
    assert result["interrupted"] is True


async def test_server_close_interrupts_client_accept_bi(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server closes while browser waits on incomingBidirectionalStreams."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                await asyncio.sleep(0.1)
                session.close(0, "")

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                """
                const reader = transport.incomingBidirectionalStreams.getReader();
                try {
                    const { done } = await reader.read();
                    return { ended: done };
                } catch (e) {
                    return { ended: true, error: e.toString() };
                }
            """,
            )

    assert isinstance(result, dict)
    assert result["ended"] is True


async def test_server_close_interrupts_client_accept_uni(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server closes while browser waits on incomingUnidirectionalStreams."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                await asyncio.sleep(0.1)
                session.close(0, "")

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                """
                const reader = transport.incomingUnidirectionalStreams.getReader();
                try {
                    const { done } = await reader.read();
                    return { ended: done };
                } catch (e) {
                    return { ended: true, error: e.toString() };
                }
            """,
            )

    assert isinstance(result, dict)
    assert result["ended"] is True


# ---------------------------------------------------------------------------
# Stream I/O after session close
# ---------------------------------------------------------------------------


async def test_server_read_on_stream_after_session_close(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """After session.close(), reading from existing RecvStream raises session error."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            send, recv = await session.accept_bi()
            await recv.read(1)
            session.close(7, "done")
            await session.wait_closed()
            try:
                await recv.read(1)
            except (
                web_transport.SessionClosed,
                web_transport.StreamClosed,
            ) as e:
                error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            try:
                await run_js(
                    port,
                    hash_b64,
                    """
                    const stream = await transport.createBidirectionalStream();
                    const writer = stream.writable.getWriter();
                    try {
                        await writer.write(new Uint8Array([1]));
                        await writer.write(new Uint8Array([1]));
                        await writer.write(new Uint8Array([1]));
                        await transport.closed;
                    } catch (e) {}
                    return true;
                """,
                )
            except Exception:
                pass

    assert error is not None


async def test_server_write_on_stream_after_session_close(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """After session.close(), writing to existing SendStream raises session error."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            send, recv = await session.accept_bi()
            session.close(7, "done")
            await session.wait_closed()
            try:
                await send.write(b"test")
            except (
                web_transport.SessionClosed,
                web_transport.StreamClosed,
            ) as e:
                error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            try:
                await run_js(
                    port,
                    hash_b64,
                    """
                    const stream = await transport.createBidirectionalStream();
                    const writer = stream.writable.getWriter();
                    await writer.write(new Uint8Array([1]));
                    try { await transport.closed; } catch (e) {}
                    return true;
                """,
                )
            except Exception:
                pass

    assert error is not None
