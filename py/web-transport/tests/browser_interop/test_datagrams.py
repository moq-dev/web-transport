"""Browser interop tests for WebTransport datagrams."""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING, Any

import pytest

import web_transport

if TYPE_CHECKING:
    from .conftest import RunJS, ServerFactory

pytestmark = pytest.mark.asyncio(loop_scope="session")


async def test_datagram_echo_text(start_server: ServerFactory, run_js: RunJS) -> None:
    """Text datagram roundtrips between Chromium and the server."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                dgram = await session.receive_datagram()
                session.send_datagram(dgram)
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const writer = transport.datagrams.writable.getWriter();
                const reader = transport.datagrams.readable.getReader();
                const payload = new TextEncoder().encode("datagram ping");
                await writer.write(payload);
                const { value } = await reader.read();
                reader.releaseLock();
                writer.releaseLock();
                return new TextDecoder().decode(value);
            """,
            )

    assert result == "datagram ping"


async def test_datagram_echo_binary(start_server: ServerFactory, run_js: RunJS) -> None:
    """All 256 byte values as datagram roundtrip."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                dgram = await session.receive_datagram()
                session.send_datagram(dgram)
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const writer = transport.datagrams.writable.getWriter();
                const reader = transport.datagrams.readable.getReader();
                const payload = new Uint8Array(256);
                for (let i = 0; i < 256; i++) payload[i] = i;
                await writer.write(payload);
                const { value } = await reader.read();
                reader.releaseLock();
                writer.releaseLock();
                return Array.from(value);
            """,
            )

    assert result == list(range(256))


async def test_datagram_server_initiates(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server sends datagram first, browser reads it."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                # Wait for browser to signal readiness
                await session.receive_datagram()
                session.send_datagram(b"server-first")
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const writer = transport.datagrams.writable.getWriter();
                const reader = transport.datagrams.readable.getReader();
                // Signal readiness to server
                await writer.write(new Uint8Array([1]));
                writer.releaseLock();
                const { value } = await reader.read();
                reader.releaseLock();
                return new TextDecoder().decode(value);
            """,
            )

    assert result == "server-first"


async def test_datagram_multiple_roundtrips(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """10 sequential request-response datagram exchanges."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                for _ in range(10):
                    dgram = await session.receive_datagram()
                    session.send_datagram(dgram)
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const writer = transport.datagrams.writable.getWriter();
                const reader = transport.datagrams.readable.getReader();
                const results = [];
                for (let i = 0; i < 10; i++) {
                    const msg = new TextEncoder().encode("msg-" + i);
                    await writer.write(msg);
                    const { value } = await reader.read();
                    results.push(new TextDecoder().decode(value));
                }
                reader.releaseLock();
                writer.releaseLock();
                return results;
            """,
            )

    assert result == [f"msg-{i}" for i in range(10)]


async def test_datagram_rapid_burst(start_server: ServerFactory, run_js: RunJS) -> None:
    """Browser sends 20 datagrams rapidly — server receives at least some (unreliable)."""
    async with start_server() as (server, port, hash_b64):
        received: list[bytes] = []

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                # Collect datagrams until the browser closes the session
                try:
                    while True:
                        dgram = await session.receive_datagram()
                        received.append(dgram)
                except web_transport.SessionClosed:
                    pass

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                const writer = transport.datagrams.writable.getWriter();
                for (let i = 0; i < 20; i++) {
                    const msg = new TextEncoder().encode("burst-" + i);
                    await writer.write(msg);
                    await new Promise(r => setTimeout(r, 10));
                }
                writer.releaseLock();
                // Wait for server to collect
                return true;
            """,
            )

    # Datagrams are unreliable — on loopback we expect most but not necessarily all
    assert len(received) >= 1


async def test_datagram_max_size_property(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """session.max_datagram_size is a positive int."""
    async with start_server() as (server, port, hash_b64):
        max_size: int = 0

        async def server_side() -> None:
            nonlocal max_size
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                max_size = session.max_datagram_size

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(port, hash_b64, "await transport.closed; return true;")

    assert max_size > 0


async def test_datagram_oversized_raises(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server sends datagram > max_datagram_size → raises DatagramTooLargeError."""
    async with start_server() as (server, port, hash_b64):
        error: web_transport.DatagramTooLargeError | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                max_size = session.max_datagram_size
                try:
                    session.send_datagram(b"\x00" * (max_size + 100))
                except web_transport.DatagramTooLargeError as e:
                    error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(port, hash_b64, "await transport.closed; return true;")

    assert error is not None


async def test_datagram_at_exact_max_size(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server sends datagram at exactly max_datagram_size bytes, browser reads it."""
    async with start_server() as (server, port, hash_b64):
        sent_size: int = 0

        async def server_side() -> None:
            nonlocal sent_size
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                max_size = session.max_datagram_size
                sent_size = max_size
                # Wait for browser to signal readiness
                await session.receive_datagram()
                session.send_datagram(b"\xab" * max_size)
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                """
                const writer = transport.datagrams.writable.getWriter();
                const reader = transport.datagrams.readable.getReader();
                // Signal readiness to server
                await writer.write(new Uint8Array([1]));
                writer.releaseLock();
                const { value } = await reader.read();
                reader.releaseLock();
                return { length: value.length };
            """,
            )

    assert isinstance(result, dict)
    assert result["length"] == sent_size
    assert sent_size > 0
