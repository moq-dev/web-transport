"""Browser interop tests for stream edge cases: FIN, reset, stop, read modes."""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING, Any

import pytest

import web_transport

if TYPE_CHECKING:
    from .conftest import RunJS, ServerFactory

pytestmark = pytest.mark.asyncio(loop_scope="session")


async def test_browser_writer_close_signals_eof(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser writer.close() causes server recv.read() to return data then EOF."""
    async with start_server() as (server, port, hash_b64):
        received: bytes = b""
        eof_bytes: bytes = b"sentinel"

        async def server_side() -> None:
            nonlocal received, eof_bytes
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    received = await recv.read()
                    eof_bytes = await recv.read()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                try { await writeAllString(stream.writable, "data"); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert received == b"data"
    assert eof_bytes == b""


async def test_server_finish_signals_eof_to_browser(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server send.finish() causes browser reader to return done: true."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                await send.write(b"payload")
                await send.finish()
                await recv.read()  # wait for browser to close
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const data = await readAllString(stream.readable);
                // Close writable to let server side complete
                const writer = stream.writable.getWriter();
                await writer.close();
                return data;
            """,
            )

    assert result == "payload"


async def test_browser_abort_with_code(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser writer.abort(42) → server recv sees StreamClosedByPeer(kind='reset', code=42)."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                await send.write(b"before-abort")
                async with send:
                    try:
                        await recv.read()
                    except web_transport.StreamClosedByPeer as e:
                        error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                const reader = stream.readable.getReader();
                await reader.read(); // wait for server to write
                let err = new WebTransportError({ message: "abort", streamErrorCode: 42 });
                await writer.abort(err);
                await transport.closed;
                return true;
            """,
            )

    assert isinstance(error, web_transport.StreamClosedByPeer)
    assert error.kind == "reset"
    assert error.code == 42


async def test_server_reset_causes_browser_read_error(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server send.reset(7) causes browser reader.read() to reject."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                send.reset(7)
                await recv.read()  # wait for browser close
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const reader = stream.readable.getReader();
                try {
                    await reader.read();
                    return { errored: false };
                } catch (e) {
                    return { errored: true, message: e.toString() };
                } finally {
                    const writer = stream.writable.getWriter();
                    await writer.close();
                }
            """,
            )

    assert isinstance(result, dict)
    assert result["errored"] is True


async def test_browser_cancel_recv_with_code(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser reader.cancel(42) → server send.write() raises StreamClosedByPeer(kind='stop', code=42)."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                # Wait a moment for the browser to cancel
                try:
                    # Write enough data to trigger the stop
                    for _ in range(10):
                        await send.write(b"x" * 65536)
                except (
                    web_transport.StreamClosedByPeer,
                    web_transport.SessionClosedByPeer,
                ) as e:
                    error = e
                try:
                    await recv.read()
                except web_transport.SessionClosed:
                    pass

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const reader = stream.readable.getReader();
                let err = new WebTransportError({ message: "cancel", streamErrorCode: 42 });
                await reader.cancel(err);
                // Close writable so server recv completes
                const writer = stream.writable.getWriter();
                try { await writer.close(); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert error is not None
    if isinstance(error, web_transport.StreamClosedByPeer):
        assert error.kind == "stop"
        assert error.code == 42


async def test_server_stop_causes_browser_write_error(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server recv.stop(7) causes browser writer.write() to reject."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    recv.stop(7)
                    await send.write(b"initial")
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                const reader = stream.readable.getReader();
                await reader.read(); // wait for server to write
                try {
                    // Write to trigger the error
                    await writer.write(new Uint8Array(65536));
                    await writer.write(new Uint8Array(65536));
                    return { errored: false };
                } catch (e) {
                    return { errored: true, message: e.toString() };
                }
            """,
            )

    assert isinstance(result, dict)
    assert result["errored"] is True


async def test_recv_read_partial(start_server: ServerFactory, run_js: RunJS) -> None:
    """Server recv.read(10) returns at most 10 bytes."""
    async with start_server() as (server, port, hash_b64):
        chunk: bytes = b""

        async def server_side() -> None:
            nonlocal chunk
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    chunk = await recv.read(10)

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const payload = new Uint8Array(100);
                for (let i = 0; i < 100; i++) payload[i] = i;
                try { await writeAll(stream.writable, payload); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert 0 < len(chunk) <= 10


async def test_recv_readexactly_success(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser sends exactly 10 bytes → server readexactly(10) succeeds."""
    async with start_server() as (server, port, hash_b64):
        data: bytes = b""

        async def server_side() -> None:
            nonlocal data
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    data = await recv.readexactly(10)

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const payload = new Uint8Array([0,1,2,3,4,5,6,7,8,9]);
                try { await writeAll(stream.writable, payload); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert data == bytes(range(10))


async def test_recv_readexactly_incomplete(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser sends 5 bytes + FIN → server readexactly(10) raises StreamIncompleteReadError."""
    async with start_server() as (server, port, hash_b64):
        error: web_transport.StreamIncompleteReadError | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    try:
                        await recv.readexactly(10)
                    except web_transport.StreamIncompleteReadError as e:
                        error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const payload = new Uint8Array([0,1,2,3,4]);
                try { await writeAll(stream.writable, payload); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert error is not None
    assert error.expected == 10
    assert len(error.partial) == 5


async def test_recv_read_with_limit_exceeded(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser sends >100 bytes → server recv.read(limit=100) raises StreamTooLongError."""
    async with start_server() as (server, port, hash_b64):
        error: web_transport.StreamTooLongError | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    try:
                        await recv.read(limit=100)
                    except web_transport.StreamTooLongError as e:
                        error = e
                session.close(0, "")
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const payload = new Uint8Array(200);
                try { await writeAll(stream.writable, payload); } catch (e) { }
                try {
                    await transport.closed; // wait for server to close after error
                } catch (e) { }
                return true;
            """,
            )

    assert error is not None
    assert error.limit == 100


async def test_recv_read_with_limit_not_exceeded(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser sends 50 bytes → server recv.read(limit=100) succeeds."""
    async with start_server() as (server, port, hash_b64):
        data: bytes = b""

        async def server_side() -> None:
            nonlocal data
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    data = await recv.read(limit=100)

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const payload = new Uint8Array(50);
                try { await writeAll(stream.writable, payload); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert len(data) == 50


async def test_recv_async_iteration(start_server: ServerFactory, run_js: RunJS) -> None:
    """Server uses async for to collect chunks from browser."""
    async with start_server() as (server, port, hash_b64):
        chunks: list[bytes] = []

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    async for chunk in recv:
                        chunks.append(chunk)

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                await writer.write(new TextEncoder().encode("a"));
                await writer.write(new TextEncoder().encode("b"));
                await writer.write(new TextEncoder().encode("c"));
                try { await writer.close(); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert len(chunks) > 0
    assert b"".join(chunks) == b"abc"


async def test_send_context_manager_finishes_on_clean_exit(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """async with send: clean exit → browser reads EOF."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    await send.write(b"context-data")
                # send is finished after context manager exits
                await recv.read()  # wait for browser close
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const data = await readAllString(stream.readable);
                const writer = stream.writable.getWriter();
                await writer.close();
                return data;
            """,
            )

    assert result == "context-data"


async def test_send_context_manager_resets_on_exception(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """async with send: + raise → browser read rejects."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                try:
                    async with send:
                        await send.write(b"partial")
                        raise ValueError("intentional error")
                except ValueError:
                    pass
                await recv.read()  # wait for browser close
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const reader = stream.readable.getReader();
                try {
                    // Read until error or done
                    while (true) {
                        const { value, done } = await reader.read();
                        if (done) break;
                    }
                    return { errored: false };
                } catch (e) {
                    return { errored: true, message: e.toString() };
                } finally {
                    const writer = stream.writable.getWriter();
                    await writer.close();
                }
            """,
            )

    assert isinstance(result, dict)
    assert result["errored"] is True


async def test_recv_context_manager_stops_if_not_eof(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """async with recv: exit before EOF → browser's subsequent write rejects."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    async with recv:
                        # Read one chunk, then exit (stop without reading to EOF)
                        await recv.read(10)
                    await send.write(b"initial")
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                const reader = stream.readable.getReader();
                // Write initial data
                await writer.write(new Uint8Array(20));
                await reader.read(); // wait for server to write
                try {
                    // Try writing more — should fail
                    for (let i = 0; i < 10; i++) {
                        await writer.write(new Uint8Array(65536));
                    }
                    return { errored: false };
                } catch (e) {
                    return { errored: true };
                }
            """,
            )

    assert isinstance(result, dict)
    assert result["errored"] is True


async def test_write_some_returns_count(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server send.write_some(data) returns int in range (0, len(data)]."""
    async with start_server() as (server, port, hash_b64):
        written: int = 0

        async def server_side() -> None:
            nonlocal written
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    written = await send.write_some(b"hello world")
                    await recv.read()  # wait for browser close

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                // Read whatever server sent
                const reader = stream.readable.getReader();
                await reader.read();
                reader.releaseLock();
                // Close writable
                const writer = stream.writable.getWriter();
                try { await writer.close(); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert 0 < written <= len(b"hello world")


async def test_read_after_eof_returns_empty(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """After EOF, recv.read() returns b'' idempotently."""
    async with start_server() as (server, port, hash_b64):
        reads: list[bytes] = []

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    # Read until EOF
                    data = await recv.read()
                    reads.append(data)
                    # Read again — should be b""
                    data2 = await recv.read()
                    reads.append(data2)
                    # And again
                    data3 = await recv.read()
                    reads.append(data3)

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                try { await writeAllString(stream.writable, "x"); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert reads[0] == b"x"
    assert reads[1] == b""
    assert reads[2] == b""


async def test_empty_write_and_finish(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server write(b'') + finish() → browser reads empty then EOF."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                await send.write(b"")
                await send.finish()
                await recv.read()  # wait for browser to close

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const data = await readAll(stream.readable);
                const writer = stream.writable.getWriter();
                try { await writer.close(); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return data.length;
            """,
            )

    assert result == 0


async def test_read_all_to_eof_default(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser writes + closes → server recv.read() (no args) returns all data."""
    async with start_server() as (server, port, hash_b64):
        received: bytes = b""

        async def server_side() -> None:
            nonlocal received
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    received = await recv.read()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                try { await writeAllString(stream.writable, "all-data-here"); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert received == b"all-data-here"


async def test_read_n_returns_less_at_eof(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser sends 5 bytes → server recv.read(1000) returns available bytes (<=5)."""
    async with start_server() as (server, port, hash_b64):
        all_received: bytes = b""

        async def server_side() -> None:
            nonlocal all_received
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    # Read with a large n — may return partial or all 5 bytes
                    chunk = await recv.read(1000)
                    assert 0 < len(chunk) <= 5
                    all_received = chunk
                    # Read remaining if any
                    rest = await recv.read(1000)
                    if rest:
                        all_received += rest

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const payload = new Uint8Array([1, 2, 3, 4, 5]);
                try { await writeAll(stream.writable, payload); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    # All data is eventually received, matching the sent payload
    assert all_received == bytes([1, 2, 3, 4, 5])


async def test_readexactly_after_eof(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """After EOF, readexactly(5) raises StreamIncompleteReadError(partial=b'')."""
    async with start_server() as (server, port, hash_b64):
        error: web_transport.StreamIncompleteReadError | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    # Read to EOF
                    await recv.read()
                    # Now readexactly should raise
                    try:
                        await recv.readexactly(5)
                    except web_transport.StreamIncompleteReadError as e:
                        error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                try { await writeAllString(stream.writable, "data"); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert error is not None
    assert error.expected == 5
    assert error.partial == b""


async def test_readexactly_zero_after_eof(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """After EOF, readexactly(0) returns b'' (no error)."""
    async with start_server() as (server, port, hash_b64):
        result_bytes: bytes = b"sentinel"

        async def server_side() -> None:
            nonlocal result_bytes
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    # Read to EOF
                    await recv.read()
                    # readexactly(0) should succeed even after EOF
                    result_bytes = await recv.readexactly(0)

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

    assert result_bytes == b""


async def test_recv_context_manager_stops_on_exception(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """async with recv: raise → browser write fails (STOP_SENDING sent)."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    try:
                        async with recv:
                            await recv.read(10)
                            raise RuntimeError("intentional")
                    except RuntimeError:
                        pass
                    await send.write(b"initial")
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                const reader = stream.readable.getReader();
                // Write initial data
                await writer.write(new Uint8Array(20));
                // Give server time to stop
                await reader.read(); // wait for server to write
                try {
                    // Try writing more — should fail
                    for (let i = 0; i < 10; i++) {
                        await writer.write(new Uint8Array(65536));
                    }
                    return { errored: false };
                } catch (e) {
                    return { errored: true };
                }
            """,
            )

    assert isinstance(result, dict)
    assert result["errored"] is True


async def test_recv_context_manager_no_stop_at_eof(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Read all data inside async with recv: → clean exit, no STOP_SENDING."""
    async with start_server() as (server, port, hash_b64):
        received: bytes = b""

        async def server_side() -> None:
            nonlocal received
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    async with recv:
                        received = await recv.read()
                    # If no STOP_SENDING was sent, this is a clean exit

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                try { await writeAllString(stream.writable, "complete-data"); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert received == b"complete-data"


# ---------------------------------------------------------------------------
# Stream reset/stop boundary codes
# ---------------------------------------------------------------------------


async def test_stream_reset_code_zero(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server resets stream with code 0, browser sees streamErrorCode=0."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                await send.write(b"\x01")
                send.reset(0)
                try:
                    await recv.read()
                except web_transport.SessionClosed:
                    pass
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const reader = stream.readable.getReader();
                try {
                    await reader.read();  // read the initial byte
                    await reader.read();  // should get RESET
                    return { errored: false };
                } catch (e) {
                    reader.releaseLock();
                    const writer = stream.writable.getWriter();
                    try { await writer.close(); } catch (_) {}
                    return {
                        errored: true,
                        code: e instanceof WebTransportError ? e.streamErrorCode : null,
                    };
                }
            """,
            )

    assert isinstance(result, dict)
    assert result["errored"] is True
    assert result["code"] == 0


async def test_stream_reset_code_255(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server resets stream with code 255, browser sees streamErrorCode=255."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                await send.write(b"\x01")
                send.reset(255)
                try:
                    await recv.read()
                except web_transport.SessionClosed:
                    pass
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const reader = stream.readable.getReader();
                try {
                    await reader.read();  // read the initial byte
                    await reader.read();  // should get RESET
                    return { errored: false };
                } catch (e) {
                    reader.releaseLock();
                    const writer = stream.writable.getWriter();
                    try { await writer.close(); } catch (_) {}
                    return {
                        errored: true,
                        code: e instanceof WebTransportError ? e.streamErrorCode : null,
                    };
                }
            """,
            )

    assert isinstance(result, dict)
    assert result["errored"] is True
    assert result["code"] == 255


async def test_stream_stop_code_zero(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser cancels reader with code 0, server sees stop code 0."""
    async with start_server() as (server, port, hash_b64):
        stop_code: int | None = -1  # sentinel

        async def server_side() -> None:
            nonlocal stop_code
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                # Also accept the keepalive stream
                send2, recv2 = await session.accept_bi()
                await send.write(b"\x01")
                stop_code = await asyncio.wait_for(send.wait_closed(), timeout=5.0)
                await send2.write(b"\x01")
                await send2.finish()
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const stream2 = await transport.createBidirectionalStream();
                const reader = stream.readable.getReader();
                await reader.read();
                let err = new WebTransportError({ message: "cancel", streamErrorCode: 0 });
                await reader.cancel(err);
                // Read from keepalive stream to let server complete
                const reader2 = stream2.readable.getReader();
                await reader2.read();
                reader2.releaseLock();
                return true;
            """,
            )

    assert stop_code == 0


async def test_stream_stop_code_255(start_server: ServerFactory, run_js: RunJS) -> None:
    """Browser cancels reader with code 255, server sees stop code 255."""
    async with start_server() as (server, port, hash_b64):
        stop_code: int | None = -1  # sentinel

        async def server_side() -> None:
            nonlocal stop_code
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                # Also accept the keepalive stream
                send2, recv2 = await session.accept_bi()
                await send.write(b"\x01")
                stop_code = await asyncio.wait_for(send.wait_closed(), timeout=5.0)
                await send2.write(b"\x01")
                await send2.finish()
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const stream2 = await transport.createBidirectionalStream();
                const reader = stream.readable.getReader();
                await reader.read();
                let err = new WebTransportError({ message: "cancel", streamErrorCode: 255 });
                await reader.cancel(err);
                // Read from keepalive stream to let server complete
                const reader2 = stream2.readable.getReader();
                await reader2.read();
                reader2.releaseLock();
                return true;
            """,
            )

    assert stop_code == 255


# ---------------------------------------------------------------------------
# Stream isolation
# ---------------------------------------------------------------------------


async def test_stream_reset_isolation(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Reset on one stream does not affect other streams on same session."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                tasks: list[asyncio.Task[None]] = []

                async def handle_stream(
                    send: web_transport.SendStream,
                    recv: web_transport.RecvStream,
                ) -> None:
                    try:
                        data = await recv.read()
                        if data == b"reset-this":
                            send.reset(33)
                        else:
                            async with send:
                                await send.write(data)
                    except (
                        web_transport.StreamClosedByPeer,
                        web_transport.StreamClosed,
                    ):
                        pass
                    except web_transport.SessionClosed:
                        pass

                try:
                    while True:
                        send, recv = await session.accept_bi()
                        tasks.append(asyncio.create_task(handle_stream(send, recv)))
                except web_transport.SessionClosed:
                    pass

                await asyncio.gather(*tasks, return_exceptions=True)

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                """
                const [echoResult, clientResetResult, serverResetResult] =
                    await Promise.allSettled([
                        (async () => {
                            const stream = await transport.createBidirectionalStream();
                            await writeAllString(stream.writable, "stream0");
                            return await readAllString(stream.readable);
                        })(),
                        (async () => {
                            const stream = await transport.createBidirectionalStream();
                            const writer = stream.writable.getWriter();
                            await writer.write(new TextEncoder().encode("data"));
                            const err = new WebTransportError({
                                message: "abort", streamErrorCode: 42
                            });
                            await writer.abort(err);
                        })(),
                        (async () => {
                            const stream = await transport.createBidirectionalStream();
                            await writeAllString(stream.writable, "reset-this");
                            const reader = stream.readable.getReader();
                            while (true) {
                                const { done } = await reader.read();
                                if (done) throw new Error("expected reset");
                            }
                        })()
                    ]);

                const echoOk = echoResult.status === "fulfilled"
                    && echoResult.value === "stream0";
                const clientResetOk = clientResetResult.status === "fulfilled";
                const srErr = serverResetResult.reason;
                const serverResetOk = serverResetResult.status === "rejected"
                    && srErr instanceof WebTransportError
                    && srErr.streamErrorCode === 33;

                return {
                    echoOk,
                    clientResetOk,
                    serverResetOk,
                    echoValue: echoResult.value,
                    srCode: srErr instanceof WebTransportError
                        ? srErr.streamErrorCode : null,
                };
            """,
            )

    assert isinstance(result, dict)
    assert result["echoOk"] is True, f"Echo failed: {result.get('echoValue')}"
    assert result["clientResetOk"] is True
    assert result["serverResetOk"] is True, f"Server reset code: {result.get('srCode')}"
