"""Browser interop tests for sequential sessions and server resilience.

Verifies that a Python WebTransport server correctly handles multiple
sequential browser connections, including recovery after misbehaving clients.
"""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING

import pytest

import web_transport
from .conftest import _webtransport_connect_js

if TYPE_CHECKING:
    from .conftest import RunJS, RunJSRaw, ServerFactory

pytestmark = pytest.mark.asyncio(loop_scope="session")


async def _echo_session(session: web_transport.Session) -> None:
    """Handle one session: echo bidi streams, uni streams, and datagrams."""

    async def echo_bidi() -> None:
        try:
            while True:
                send, recv = await session.accept_bi()
                try:
                    data = await recv.read()
                    await send.write(data)
                    await send.finish()
                except (web_transport.StreamError, web_transport.SessionError):
                    pass
        except web_transport.SessionError:
            pass

    async def echo_uni() -> None:
        try:
            while True:
                recv = await session.accept_uni()
                try:
                    data = await recv.read()
                    send = await session.open_uni()
                    await send.write(data)
                    await send.finish()
                except (web_transport.StreamError, web_transport.SessionError):
                    pass
        except web_transport.SessionError:
            pass

    async def echo_datagrams() -> None:
        try:
            while True:
                dgram = await session.receive_datagram()
                session.send_datagram(dgram)
        except (web_transport.SessionError, web_transport.DatagramError):
            pass

    tasks = [
        asyncio.create_task(echo_bidi()),
        asyncio.create_task(echo_uni()),
        asyncio.create_task(echo_datagrams()),
    ]
    try:
        await session.wait_closed()
    finally:
        for t in tasks:
            t.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)


async def test_sequential_browser_sessions_bidi_echo(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """3 sequential browser connections, each echoes via bidi stream."""
    async with start_server() as (server, port, hash_b64):
        session_count = 0

        async def server_side() -> None:
            nonlocal session_count
            for _ in range(3):
                request = await server.accept()
                assert request is not None
                session = await request.accept()
                await _echo_session(session)
                session_count += 1

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            results = []
            for i in range(3):
                r = await run_js(
                    port,
                    hash_b64,
                    f"""
                    const stream = await transport.createBidirectionalStream();
                    await writeAllString(stream.writable, "session-{i}");
                    return await readAllString(stream.readable);
                """,
                )
                results.append(r)

    assert results == [f"session-{i}" for i in range(3)]
    assert session_count == 3


async def test_sequential_browser_sessions_mixed_types(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """3 sessions: bidi, uni, datagram — each uses a different transport mode."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            for _ in range(3):
                request = await server.accept()
                assert request is not None
                session = await request.accept()
                await _echo_session(session)

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())

            # Session 1: bidi
            r1 = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                await writeAllString(stream.writable, "bidi-data");
                return await readAllString(stream.readable);
            """,
            )

            # Session 2: uni
            r2 = await run_js(
                port,
                hash_b64,
                """
                const sendStream = await transport.createUnidirectionalStream();
                await writeAllString(sendStream, "uni-data");
                const reader = transport.incomingUnidirectionalStreams.getReader();
                const { value: recvStream } = await reader.read();
                reader.releaseLock();
                return await readAllString(recvStream);
            """,
            )

            # Session 3: datagram
            r3 = await run_js(
                port,
                hash_b64,
                """
                const writer = transport.datagrams.writable.getWriter();
                const reader = transport.datagrams.readable.getReader();
                await writer.write(new TextEncoder().encode("dg-data"));
                const { value } = await reader.read();
                reader.releaseLock();
                writer.releaseLock();
                return new TextDecoder().decode(value);
            """,
            )

    assert r1 == "bidi-data"
    assert r2 == "uni-data"
    assert r3 == "dg-data"


async def test_server_survives_browser_abort(
    start_server: ServerFactory, run_js_raw: RunJSRaw, run_js: RunJS
) -> None:
    """Browser writes partial + closes abruptly → next browser echoes fine."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            # Session 1: browser aborts
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await _echo_session(session)
            # Session 2: clean echo
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await _echo_session(session)

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())

            # Misbehaving browser: write partial, close abruptly
            await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                await transport.ready;
                const stream = await transport.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                await writer.write(new TextEncoder().encode("partial"));
                // Close transport abruptly without finishing stream
                transport.close({{closeCode: 1, reason: "abort"}});
                await transport.closed;
                return true;
            """)

            # Clean browser: should work fine
            result = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                await writeAllString(stream.writable, "clean-echo");
                return await readAllString(stream.readable);
            """,
            )

    assert result == "clean-echo"


async def test_server_survives_browser_stream_reset(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser resets stream → next browser echoes fine."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            # Session 1: browser resets stream
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await _echo_session(session)
            # Session 2: clean echo
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await _echo_session(session)

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())

            # Misbehaving browser: reset stream
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                let err = new WebTransportError({ message: "reset", streamErrorCode: 99 });
                await writer.abort(err);
                return true;
            """,
            )

            # Clean browser: should work fine
            result = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                await writeAllString(stream.writable, "after-reset");
                return await readAllString(stream.readable);
            """,
            )

    assert result == "after-reset"


async def test_server_survives_browser_idle_disconnect(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser connects, does nothing, closes → next browser echoes fine."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            # Session 1: idle browser
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await _echo_session(session)
            # Session 2: clean echo
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await _echo_session(session)

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())

            # Idle browser: connect and immediately close
            await run_js(
                port,
                hash_b64,
                """
                // Do nothing — transport auto-closes in finally
                return true;
            """,
            )

            # Clean browser: should work fine
            result = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                await writeAllString(stream.writable, "after-idle");
                return await readAllString(stream.readable);
            """,
            )

    assert result == "after-idle"


async def test_server_survives_many_misbehaviors(
    start_server: ServerFactory, run_js_raw: RunJSRaw, run_js: RunJS
) -> None:
    """Chain of different bad behaviors, then verify a clean session works."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            # Accept 4 sessions (3 bad + 1 good)
            for _ in range(4):
                request = await server.accept()
                assert request is not None
                session = await request.accept()
                await _echo_session(session)

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())

            # Bad 1: idle disconnect
            await run_js(
                port,
                hash_b64,
                "return true;",
            )

            # Bad 2: stream reset
            await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                let err = new WebTransportError({ message: "reset", streamErrorCode: 1 });
                await writer.abort(err);
                return true;
            """,
            )

            # Bad 3: abrupt close with partial data
            await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                await transport.ready;
                const stream = await transport.createBidirectionalStream();
                const writer = stream.writable.getWriter();
                await writer.write(new TextEncoder().encode("abandoned"));
                transport.close({{closeCode: 99, reason: "bail"}});
                await transport.closed;
                return true;
            """)

            # Good: clean echo should succeed
            result = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                await writeAllString(stream.writable, "survived");
                return await readAllString(stream.readable);
            """,
            )

    assert result == "survived"
