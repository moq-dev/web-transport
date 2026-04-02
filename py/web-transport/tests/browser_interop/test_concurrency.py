"""Browser interop tests for concurrent operations and high load."""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING, Any

import pytest

import web_transport

from .conftest import _webtransport_connect_js

if TYPE_CHECKING:
    from .conftest import RunJS, RunJSRaw, ServerFactory

pytestmark = pytest.mark.asyncio(loop_scope="session")


async def test_concurrent_bidi_and_uni_streams(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """3 bidi + 3 uni streams simultaneously, all echo/deliver correctly."""
    async with start_server() as (server, port, hash_b64):
        uni_received: list[bytes] = []

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:

                async def handle_bidi_streams() -> None:
                    for _ in range(3):
                        send, recv = await session.accept_bi()
                        async with send:
                            data = await recv.read()
                            await send.write(data)

                async def handle_uni_streams() -> None:
                    for _ in range(3):
                        recv = await session.accept_uni()
                        data = await recv.read()
                        uni_received.append(data)

                async with asyncio.TaskGroup() as inner_tg:
                    inner_tg.create_task(handle_bidi_streams())
                    inner_tg.create_task(handle_uni_streams())

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const bidiPromises = [];
                const uniPromises = [];
                for (let i = 0; i < 3; i++) {
                    bidiPromises.push((async () => {
                        const stream = await transport.createBidirectionalStream();
                        try { await writeAllString(stream.writable, "bidi-" + i); } catch (e) { }
                        return await readAllString(stream.readable);
                    })());
                    uniPromises.push((async () => {
                        const stream = await transport.createUnidirectionalStream();
                        try { await writeAllString(stream, "uni-" + i); } catch (e) { }
                    })());
                }
                const bidiResults = await Promise.all(bidiPromises);
                await Promise.all(uniPromises);
                try { await transport.closed; } catch (e) { }
                return bidiResults.sort();
            """,
            )

    expected_bidi = sorted(f"bidi-{i}" for i in range(3))
    assert result == expected_bidi
    assert sorted(uni_received) == sorted(f"uni-{i}".encode() for i in range(3))


async def test_streams_and_datagrams_simultaneously(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Bidi stream echo + datagram echo running concurrently."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:

                async def echo_stream() -> None:
                    send, recv = await session.accept_bi()
                    async with send:
                        data = await recv.read()
                        await send.write(data)

                async def echo_datagram() -> None:
                    dgram = await session.receive_datagram()
                    session.send_datagram(dgram)

                async with asyncio.TaskGroup() as inner_tg:
                    inner_tg.create_task(echo_stream())
                    inner_tg.create_task(echo_datagram())
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                """
                // Run stream and datagram concurrently
                const [streamResult, dgResult] = await Promise.all([
                    (async () => {
                        const stream = await transport.createBidirectionalStream();
                        await writeAllString(stream.writable, "stream-data");
                        return await readAllString(stream.readable);
                    })(),
                    (async () => {
                        const writer = transport.datagrams.writable.getWriter();
                        const reader = transport.datagrams.readable.getReader();
                        await writer.write(new TextEncoder().encode("dg-data"));
                        const { value } = await reader.read();
                        reader.releaseLock();
                        writer.releaseLock();
                        return new TextDecoder().decode(value);
                    })(),
                ]);
                return { stream: streamResult, datagram: dgResult };
            """,
            )

    assert isinstance(result, dict)
    assert result["stream"] == "stream-data"
    assert result["datagram"] == "dg-data"


async def test_many_streams(start_server: ServerFactory, run_js: RunJS) -> None:
    """20 bidi streams, each with small payload, all echo correctly."""
    n = 20
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                for _ in range(n):
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
                f"""
                const promises = [];
                for (let i = 0; i < {n}; i++) {{
                    promises.push((async () => {{
                        const stream = await transport.createBidirectionalStream();
                        const msg = "s-" + i;
                        await writeAllString(stream.writable, msg);
                        return await readAllString(stream.readable);
                    }})());
                }}
                const results = await Promise.all(promises);
                return results.sort();
            """,
            )

    expected = sorted(f"s-{i}" for i in range(n))
    assert result == expected


async def test_large_concurrent_transfers(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """3 bidi streams each transferring 100 KB concurrently."""
    size = 100 * 1024
    n = 3
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                for _ in range(n):
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
                f"""
                const promises = [];
                for (let i = 0; i < {n}; i++) {{
                    promises.push((async () => {{
                        const stream = await transport.createBidirectionalStream();
                        const payload = new Uint8Array({size});
                        for (let j = 0; j < payload.length; j++) payload[j] = (i + j) & 0xff;
                        await writeAll(stream.writable, payload);
                        const echoed = await readAll(stream.readable);
                        return echoed.length;
                    }})());
                }}
                return await Promise.all(promises);
            """,
            )

    assert result == [size] * n


async def test_rapid_open_close_streams(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Open and immediately close 10 streams in sequence, no errors."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                for _ in range(10):
                    try:
                        send, recv = await session.accept_bi()
                        async with send:
                            await recv.read()
                    except (
                        web_transport.StreamClosed,
                        web_transport.SessionClosed,
                    ):
                        pass

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                for (let i = 0; i < 10; i++) {
                    const stream = await transport.createBidirectionalStream();
                    const writer = stream.writable.getWriter();
                    try { await writer.close(); } catch (e) { }
                }
                try { await transport.closed; } catch (e) { }
                return true;
            """,
            )

    assert result is True


async def test_sequential_sessions_same_server(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """3 sequential browser connections to the same server."""
    async with start_server() as (server, port, hash_b64):
        session_count = 0

        async def server_side() -> None:
            nonlocal session_count
            for _ in range(3):
                request = await server.accept()
                assert request is not None
                session = await request.accept()
                async with session:
                    send, recv = await session.accept_bi()
                    async with send:
                        data = await recv.read()
                        await send.write(data)
                    await session.wait_closed()
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
                    const echo = await readAllString(stream.readable);
                    return echo;
                """,
                )
                results.append(r)

    assert results == [f"session-{i}" for i in range(3)]
    assert session_count == 3


async def test_interleaved_stream_and_datagram(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Alternating: datagram, bidi stream, datagram, bidi stream."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                # datagram 1
                dg1 = await session.receive_datagram()
                session.send_datagram(dg1)
                # bidi 1
                send1, recv1 = await session.accept_bi()
                async with send1:
                    data1 = await recv1.read()
                    await send1.write(data1)
                # datagram 2
                dg2 = await session.receive_datagram()
                session.send_datagram(dg2)
                # bidi 2
                send2, recv2 = await session.accept_bi()
                async with send2:
                    data2 = await recv2.read()
                    await send2.write(data2)
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const dgWriter = transport.datagrams.writable.getWriter();
                const dgReader = transport.datagrams.readable.getReader();
                const results = [];

                // datagram 1
                await dgWriter.write(new TextEncoder().encode("dg1"));
                let { value } = await dgReader.read();
                results.push("dg:" + new TextDecoder().decode(value));

                // bidi 1
                let stream = await transport.createBidirectionalStream();
                await writeAllString(stream.writable, "bi1");
                results.push("bi:" + await readAllString(stream.readable));

                // datagram 2
                await dgWriter.write(new TextEncoder().encode("dg2"));
                ({ value } = await dgReader.read());
                results.push("dg:" + new TextDecoder().decode(value));

                // bidi 2
                stream = await transport.createBidirectionalStream();
                await writeAllString(stream.writable, "bi2");
                results.push("bi:" + await readAllString(stream.readable));

                dgReader.releaseLock();
                dgWriter.releaseLock();
                return results;
            """,
            )

    assert result == ["dg:dg1", "bi:bi1", "dg:dg2", "bi:bi2"]


async def test_rapid_stream_creation(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """50 bidi streams opened rapidly, each with unique payload, all echo correctly."""
    n = 50
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                for _ in range(n):
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
                f"""
                const N = {n};
                const promises = [];
                for (let i = 0; i < N; i++) {{
                    promises.push((async () => {{
                        const stream = await transport.createBidirectionalStream();
                        await writeAll(stream.writable, new Uint8Array([i % 256]));
                        const echoed = await readAll(stream.readable);
                        return echoed.length === 1 && echoed[0] === (i % 256);
                    }})());
                }}
                const results = await Promise.all(promises);
                return results.every(r => r === true);
            """,
            )

    assert result is True


async def test_server_close_while_client_creating_streams(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Server closes session after brief delay while browser tries to open 20 streams."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                try:
                    send, recv = await session.accept_bi()
                    async with send:
                        await recv.read()
                except web_transport.SessionClosed:
                    return
                await asyncio.sleep(0.1)
                session.close(99, "closing")

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                """
                const N = 20;
                const promises = [];
                for (let i = 0; i < N; i++) {
                    promises.push((async () => {
                        try {
                            const stream = await transport.createBidirectionalStream();
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
                try { await transport.closed; } catch (e) {}
                return { succeeded: succeeded, failed: failed };
            """,
            )

    assert isinstance(result, dict)
    assert result["succeeded"] >= 1


async def test_bidirectional_open(start_server: ServerFactory, run_js: RunJS) -> None:
    """Both browser and server open 5 bidi streams concurrently."""
    n = 5
    async with start_server() as (server, port, hash_b64):
        server_errors: list[str] = []

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:

                async def server_open(i: int) -> None:
                    send, recv = await session.open_bi()
                    async with send:
                        await send.write(f"s{i}".encode())
                        await send.finish()
                        data = await recv.read()
                        if data != f"s{i}".encode():
                            server_errors.append(
                                f"stream {i}: expected s{i}, got {data!r}"
                            )

                async def server_accept() -> None:
                    for _ in range(n):
                        send, recv = await session.accept_bi()
                        async with send:
                            data = await recv.read()
                            await send.write(data)

                async with asyncio.TaskGroup() as inner_tg:
                    for i in range(n):
                        inner_tg.create_task(server_open(i))
                    inner_tg.create_task(server_accept())

                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js(
                port,
                hash_b64,
                f"""
                const N = {n};
                const errors = [];

                const clientOpenPromises = [];
                for (let i = 0; i < N; i++) {{
                    clientOpenPromises.push((async () => {{
                        const stream = await transport.createBidirectionalStream();
                        const msg = "c" + i;
                        await writeAllString(stream.writable, msg);
                        const echo = await readAllString(stream.readable);
                        if (echo !== msg) errors.push("client " + i + ": got " + echo);
                    }})());
                }}

                const serverStreamReader = transport.incomingBidirectionalStreams.getReader();
                const clientAcceptPromises = [];
                for (let i = 0; i < N; i++) {{
                    clientAcceptPromises.push((async () => {{
                        const {{ value: stream, done }} = await serverStreamReader.read();
                        if (done) {{ errors.push("incoming ended early"); return; }}
                        const data = await readAllString(stream.readable);
                        await writeAllString(stream.writable, data);
                    }})());
                }}

                await Promise.all([...clientOpenPromises, ...clientAcceptPromises]);
                serverStreamReader.releaseLock();
                return {{ success: errors.length === 0, errors: errors }};
            """,
            )

    assert isinstance(result, dict)
    assert result["success"] is True, f"Browser errors: {result.get('errors')}"
    assert server_errors == [], f"Server errors: {server_errors}"


async def test_multiple_concurrent_sessions(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """Multiple browser sessions concurrently to same server, each echoing unique data."""
    n = 5
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            for _ in range(n):
                request = await server.accept()
                assert request is not None
                session = await request.accept()
                async with session:
                    send, recv = await session.accept_bi()
                    async with send:
                        data = await recv.read()
                        await send.write(data)
                    await session.wait_closed()

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js_raw(f"""
                {setup}
                const N = {n};
                const promises = [];
                for (let i = 0; i < N; i++) {{
                    promises.push((async () => {{
                        const t = new WebTransport(url, transportOptions);
                        await t.ready;
                        const stream = await t.createBidirectionalStream();
                        await writeAllString(stream.writable, "sess-" + i);
                        const echo = await readAllString(stream.readable);
                        t.close();
                        await t.closed;
                        return {{ i: i, echo: echo }};
                    }})());
                }}
                const results = await Promise.all(promises);
                const failed = results.filter(r => r.echo !== "sess-" + r.i);
                return {{ success: failed.length === 0, failed: failed }};
            """)

    assert isinstance(result, dict)
    assert result["success"] is True, f"Failed: {result.get('failed')}"
