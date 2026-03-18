"""Browser interop tests for WebTransport bidirectional streams."""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING, Any

import pytest

if TYPE_CHECKING:
    from .conftest import RunJS, ServerFactory

pytestmark = pytest.mark.asyncio(loop_scope="session")


async def test_bidi_echo_text(start_server: ServerFactory, run_js: RunJS) -> None:
    """UTF-8 text roundtrips through a bidi stream."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    data = await recv.read()
                    await send.write(data)
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                await writeAllString(stream.writable, "hello from chromium");
                const echo = await readAllString(stream.readable);
                return echo;
            """,
            )

    assert result == "hello from chromium"


async def test_bidi_echo_binary_all_byte_values(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """All 256 byte values survive the full browser-server-browser roundtrip."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    data = await recv.read()
                    await send.write(data)
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const payload = new Uint8Array(256);
                for (let i = 0; i < 256; i++) payload[i] = i;
                await writeAll(stream.writable, payload);
                const echoed = await readAll(stream.readable);
                return Array.from(echoed);
            """,
            )

    assert result == list(range(256))


async def test_bidi_echo_large_payload(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """1 MB payload through a bidi stream."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    data = await recv.read()
                    await send.write(data)
                await session.wait_closed()

        size = 1024 * 1024  # 1 MB
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                f"""
                const stream = await transport.createBidirectionalStream();
                const payload = new Uint8Array({size});
                for (let i = 0; i < payload.length; i++) payload[i] = i & 0xff;
                await writeAll(stream.writable, payload);
                const echoed = await readAll(stream.readable);
                if (echoed.length !== {size})
                    throw new Error("Size mismatch: " + echoed.length);
                // Spot-check instead of transferring full array
                const result = {{
                    length: echoed.length,
                    first: echoed[0],
                    mid: echoed[{size // 2}],
                    last: echoed[{size - 1}],
                }};
                return result;
            """,
            )

    assert isinstance(result, dict)
    assert result["length"] == size
    assert result["first"] == 0
    assert result["mid"] == (size // 2) & 0xFF
    assert result["last"] == (size - 1) & 0xFF


async def test_bidi_empty_stream(start_server: ServerFactory, run_js: RunJS) -> None:
    """Browser opens bidi, immediately closes writable — server reads empty bytes."""
    async with start_server() as (server, port, hash_b64):
        received: bytes = b"sentinel"

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
                const writer = stream.writable.getWriter();
                try { await writer.close(); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert received == b""


async def test_bidi_server_opens_stream(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server opens bidi stream, writes data, browser reads and writes back."""
    async with start_server() as (server, port, hash_b64):
        server_received: bytes = b""

        async def server_side() -> None:
            nonlocal server_received
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.open_bi()
                async with send:
                    await send.write(b"from server")
                    await send.finish()
                    server_received = await recv.read()
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const reader = transport.incomingBidirectionalStreams.getReader();
                const { value: stream } = await reader.read();
                reader.releaseLock();
                const data = await readAllString(stream.readable);
                await writeAllString(stream.writable, "from browser");
                return data;
            """,
            )

    assert result == "from server"
    assert server_received == b"from browser"


async def test_bidi_multiple_sequential(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """5 sequential bidi streams, each with unique payload."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                for _ in range(5):
                    send, recv = await session.accept_bi()
                    async with send:
                        data = await recv.read()
                        await send.write(data)
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const results = [];
                for (let i = 0; i < 5; i++) {
                    const stream = await transport.createBidirectionalStream();
                    const msg = "message-" + i;
                    await writeAllString(stream.writable, msg);
                    const echo = await readAllString(stream.readable);
                    results.push(echo);
                }
                return results;
            """,
            )

    assert result == [f"message-{i}" for i in range(5)]


async def test_bidi_multiple_concurrent(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """5 bidi streams opened concurrently, server echoes all."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                for _ in range(5):
                    send, recv = await session.accept_bi()
                    async with send:
                        data = await recv.read()
                        await send.write(data)
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const promises = [];
                for (let i = 0; i < 5; i++) {
                    promises.push((async () => {
                        const stream = await transport.createBidirectionalStream();
                        const msg = "concurrent-" + i;
                        await writeAllString(stream.writable, msg);
                        return await readAllString(stream.readable);
                    })());
                }
                const results = await Promise.all(promises);
                return results.sort();
            """,
            )

    expected = sorted(f"concurrent-{i}" for i in range(5))
    assert result == expected


async def test_bidi_browser_writes_multiple_chunks(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser writes 3 chunks, server reads concatenation."""
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
                    await send.write(b"ok")
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                await writer.write(new TextEncoder().encode("aaa"));
                await writer.write(new TextEncoder().encode("bbb"));
                await writer.write(new TextEncoder().encode("ccc"));
                await writer.close();
                const echo = await readAllString(stream.readable);
                return echo;
            """,
            )

    assert received == b"aaabbbccc"
    assert result == "ok"


async def test_bidi_server_writes_multiple_chunks(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server writes 3 chunks, browser reads concatenation."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    await send.write(b"chunk1-")
                    await send.write(b"chunk2-")
                    await send.write(b"chunk3")
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                // Close writable so server can proceed
                const writer = stream.writable.getWriter();
                await writer.close();
                const data = await readAllString(stream.readable);
                return data;
            """,
            )

    assert result == "chunk1-chunk2-chunk3"


async def test_bidi_half_close_then_read(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser closes writable (FIN), then reads response from server."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    data = await recv.read()
                    await send.write(b"got: " + data)
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                await writeAllString(stream.writable, "request");
                const echo = await readAllString(stream.readable);
                return echo;
            """,
            )

    assert result == "got: request"


async def test_bidi_interleaved_request_response(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Alternating read/write on same bidi stream."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    # Read "ping", write "pong"
                    data1 = await recv.read(1024)
                    assert data1 == b"ping"
                    await send.write(b"pong")
                    # Read "ping2", write "pong2"
                    data2 = await recv.read(1024)
                    assert data2 == b"ping2"
                    await send.write(b"pong2")
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                const reader = stream.readable.getReader();

                // Send "ping", read "pong"
                await writer.write(new TextEncoder().encode("ping"));
                let { value } = await reader.read();
                const resp1 = new TextDecoder().decode(value);

                // Send "ping2", read "pong2"
                await writer.write(new TextEncoder().encode("ping2"));
                ({ value } = await reader.read());
                const resp2 = new TextDecoder().decode(value);

                writer.releaseLock();
                reader.releaseLock();
                return [resp1, resp2];
            """,
            )

    assert result == ["pong", "pong2"]


async def test_bidi_server_opens_multiple_streams(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server opens 3 bidi streams, sends unique data on each, browser reads all 3."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                for i in range(3):
                    send, recv = await session.open_bi()
                    async with send:
                        await send.write(bytes([i]))
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const reader = transport.incomingBidirectionalStreams.getReader();
                const values = [];
                for (let i = 0; i < 3; i++) {
                    const { value: stream, done } = await reader.read();
                    if (done) break;
                    const data = await readAll(stream.readable);
                    values.push(data[0]);
                }
                reader.releaseLock();
                values.sort((a, b) => a - b);
                return Array.from(values);
            """,
            )

    assert result == [0, 1, 2]


async def test_bidi_server_stream_priority(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server opens 3 bidi streams with different priorities, sends data on each."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                for i in range(3):
                    send, recv = await session.open_bi()
                    send.priority = i
                    async with send:
                        await send.write(f"prio{i}".encode())
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const reader = transport.incomingBidirectionalStreams.getReader();
                const messages = [];
                for (let i = 0; i < 3; i++) {
                    const { value: stream, done } = await reader.read();
                    if (done) break;
                    const text = await readAllString(stream.readable);
                    messages.push(text);
                }
                reader.releaseLock();
                return messages.sort();
            """,
            )

    assert result == ["prio0", "prio1", "prio2"]
