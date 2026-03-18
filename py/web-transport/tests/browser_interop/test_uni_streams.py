"""Browser interop tests for WebTransport unidirectional streams."""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING, Any

import pytest

if TYPE_CHECKING:
    from .conftest import RunJS, ServerFactory

pytestmark = pytest.mark.asyncio(loop_scope="session")


async def test_uni_browser_to_server(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser creates uni stream, writes, closes — server reads."""
    async with start_server() as (server, port, hash_b64):
        received: bytes = b""

        async def server_side() -> None:
            nonlocal received
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                recv = await session.accept_uni()
                received = await recv.read()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createUnidirectionalStream();
                try { await writeAllString(stream, "uni hello"); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert received == b"uni hello"


async def test_uni_server_to_browser(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server opens uni stream, writes, finishes — browser reads."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send = await session.open_uni()
                async with send:
                    await send.write(b"from server uni")
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const reader = transport.incomingUnidirectionalStreams.getReader();
                const { value: stream } = await reader.read();
                reader.releaseLock();
                return await readAllString(stream);
            """,
            )

    assert result == "from server uni"


async def test_uni_browser_to_server_large_payload(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """512 KB via unidirectional stream browser to server."""
    size = 512 * 1024
    async with start_server() as (server, port, hash_b64):
        received_len: int = 0

        async def server_side() -> None:
            nonlocal received_len
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                recv = await session.accept_uni()
                data = await recv.read()
                received_len = len(data)

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                f"""
                const stream = await transport.createUnidirectionalStream();
                const payload = new Uint8Array({size});
                for (let i = 0; i < payload.length; i++) payload[i] = i & 0xff;
                try {{ await writeAll(stream, payload); }} catch (e) {{ }}
                try {{ await transport.closed; }} catch (e) {{ }}
                return true;
            """,
            )

    assert received_len == size


async def test_uni_multiple_browser_to_server(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """5 uni streams from browser to server, each with unique payload."""
    async with start_server() as (server, port, hash_b64):
        received: list[bytes] = []

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                for _ in range(5):
                    recv = await session.accept_uni()
                    data = await recv.read()
                    received.append(data)
                    await recv.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                for (let i = 0; i < 5; i++) {
                    const stream = await transport.createUnidirectionalStream();
                    try { await writeAllString(stream, "uni-" + i); } catch (e) { }
                }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert sorted(received) == sorted(f"uni-{i}".encode() for i in range(5))


async def test_uni_multiple_server_to_browser(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server opens 5 uni streams, browser reads all."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                for i in range(5):
                    send = await session.open_uni()
                    async with send:
                        await send.write(f"srv-{i}".encode())
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const reader = transport.incomingUnidirectionalStreams.getReader();
                const results = [];
                for (let i = 0; i < 5; i++) {
                    const { value: stream } = await reader.read();
                    const text = await readAllString(stream);
                    results.push(text);
                }
                reader.releaseLock();
                return results.sort();
            """,
            )

    expected = sorted(f"srv-{i}" for i in range(5))
    assert result == expected


async def test_uni_browser_to_server_binary(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """All 256 byte values via unidirectional stream browser to server."""
    async with start_server() as (server, port, hash_b64):
        received: bytes = b""

        async def server_side() -> None:
            nonlocal received
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                recv = await session.accept_uni()
                received = await recv.read()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createUnidirectionalStream();
                const payload = new Uint8Array(256);
                for (let i = 0; i < 256; i++) payload[i] = i;
                try { await writeAll(stream, payload); } catch (e) { }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert received == bytes(range(256))


async def test_uni_server_to_browser_binary(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """All 256 byte values via unidirectional stream server to browser."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send = await session.open_uni()
                async with send:
                    await send.write(bytes(range(256)))
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const reader = transport.incomingUnidirectionalStreams.getReader();
                const { value: stream } = await reader.read();
                reader.releaseLock();
                const data = await readAll(stream);
                return Array.from(data);
            """,
            )

    assert result == list(range(256))


async def test_uni_server_to_browser_large_payload(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server sends 64KB pattern data via uni stream, browser verifies."""
    size = 65536
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                send = await session.open_uni()
                async with send:
                    data = bytes(i % 251 for i in range(size))
                    await send.write(data)
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                f"""
                const reader = transport.incomingUnidirectionalStreams.getReader();
                const {{ value: stream }} = await reader.read();
                reader.releaseLock();
                const data = await readAll(stream);
                let ok = data.length === {size};
                for (let i = 0; ok && i < data.length; i++) {{
                    if (data[i] !== (i % 251)) ok = false;
                }}
                return {{ length: data.length, ok: ok }};
            """,
            )

    assert isinstance(result, dict)
    assert result["length"] == size
    assert result["ok"] is True
