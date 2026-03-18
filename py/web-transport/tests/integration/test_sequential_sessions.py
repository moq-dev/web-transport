"""Integration tests for sequential sessions on a long-lived server.

Tests that a server running a background accept loop correctly handles
multiple independent client connections arriving one after another. Each
client connects, performs some work (streams, datagrams), disconnects,
and the next client repeats — exercising QUIC connection lifecycle cleanup,
session handler teardown, and server state isolation between connections.

The "misbehaving client" section verifies that a client doing anything
unexpected (resetting streams, stopping reads, dropping sessions, etc.)
never corrupts the server's ability to serve subsequent well-behaved clients.
"""

import asyncio
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager

import pytest

import web_transport


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


async def _handle_session(session: web_transport.Session) -> None:
    """Full echo handler: bidi streams, uni streams, and datagrams.

    Each stream is handled independently — a misbehaving stream (reset, stop,
    etc.) does not break the handler for subsequent streams in the same session.
    """

    async def echo_one_bidi(
        send: web_transport.SendStream, recv: web_transport.RecvStream
    ) -> None:
        try:
            data = await recv.read()
            await send.write(data)
            await send.finish()
        except (web_transport.StreamError, web_transport.SessionError):
            pass

    async def echo_bidi() -> None:
        try:
            while True:
                send, recv = await session.accept_bi()
                asyncio.create_task(echo_one_bidi(send, recv))
        except web_transport.SessionError:
            pass

    async def echo_one_uni(recv: web_transport.RecvStream) -> None:
        try:
            data = await recv.read()
            send = await session.open_uni()
            await send.write(data)
            await send.finish()
        except (web_transport.StreamError, web_transport.SessionError):
            pass

    async def echo_uni() -> None:
        try:
            while True:
                recv = await session.accept_uni()
                asyncio.create_task(echo_one_uni(recv))
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


async def _accept_loop(
    server: web_transport.Server,
    ready: asyncio.Event,
) -> None:
    """Accept incoming sessions and spawn echo handlers."""
    tasks: set[asyncio.Task[None]] = set()
    try:
        ready.set()
        async for request in server:
            try:
                session = await request.accept()
            except web_transport.SessionError:
                continue
            task = asyncio.create_task(_handle_session(session))
            tasks.add(task)
            task.add_done_callback(tasks.discard)
    except asyncio.CancelledError:
        pass
    finally:
        for t in tasks:
            t.cancel()


@asynccontextmanager
async def _echo_server(
    cert: bytes, key: bytes
) -> AsyncIterator[tuple[web_transport.Server, int]]:
    """Start a long-lived echo server and yield ``(server, port)``."""
    async with web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    ) as server:
        _, port = server.local_addr
        ready = asyncio.Event()
        accept_task = asyncio.create_task(_accept_loop(server, ready))
        await ready.wait()
        try:
            yield server, port
        finally:
            accept_task.cancel()
            try:
                await accept_task
            except asyncio.CancelledError:
                pass


async def _assert_bidi_echo(
    port: int, cert_hash: bytes, payload: bytes = b"health-check"
) -> None:
    """Connect a fresh client and verify bidi echo works end-to-end."""
    async with web_transport.Client(
        server_certificate_hashes=[cert_hash],
    ) as client:
        session = await client.connect(f"https://[::1]:{port}")
        send, recv = await session.open_bi()
        await send.write(payload)
        await send.finish()
        data = await recv.read()
        assert data == payload
        session.close()


# ---------------------------------------------------------------------------
# Well-behaved client tests
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_sequential_clients_bidi_echo(
    self_signed_cert: tuple[bytes, bytes],
    cert_hash: bytes,
) -> None:
    """Independent clients connect one-at-a-time and echo via bidi streams."""
    cert, key = self_signed_cert

    async with _echo_server(cert, key) as (_, port):
        for i in range(5):
            await _assert_bidi_echo(port, cert_hash, f"bidi-{i}".encode())


@pytest.mark.asyncio
async def test_sequential_clients_uni_echo(
    self_signed_cert: tuple[bytes, bytes],
    cert_hash: bytes,
) -> None:
    """Independent clients connect one-at-a-time and echo via uni streams."""
    cert, key = self_signed_cert

    async with _echo_server(cert, key) as (_, port):
        for i in range(5):
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash],
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                send = await session.open_uni()
                payload = f"uni-{i}".encode()
                await send.write(payload)
                await send.finish()
                recv = await session.accept_uni()
                data = await recv.read()
                assert data == payload
                session.close()


@pytest.mark.asyncio
async def test_sequential_clients_datagram_echo(
    self_signed_cert: tuple[bytes, bytes],
    cert_hash: bytes,
) -> None:
    """Independent clients connect one-at-a-time and echo datagrams."""
    cert, key = self_signed_cert

    async with _echo_server(cert, key) as (_, port):
        for i in range(5):
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash],
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                payload = f"dgram-{i}".encode()
                session.send_datagram(payload)
                data = await session.receive_datagram()
                assert data == payload
                session.close()


@pytest.mark.asyncio
async def test_single_client_sequential_connections(
    self_signed_cert: tuple[bytes, bytes],
    cert_hash: bytes,
) -> None:
    """One Client instance makes multiple sequential connections.

    The same QUIC endpoint is reused for each connect(); verifies that
    internal endpoint state is properly cleaned up between sessions.
    """
    cert, key = self_signed_cert

    async with _echo_server(cert, key) as (_, port):
        async with web_transport.Client(
            server_certificate_hashes=[cert_hash],
        ) as client:
            for i in range(5):
                session = await client.connect(f"https://[::1]:{port}")
                send, recv = await session.open_bi()
                payload = f"reuse-{i}".encode()
                await send.write(payload)
                await send.finish()
                data = await recv.read()
                assert data == payload
                session.close()


@pytest.mark.asyncio
async def test_multiple_streams_per_sequential_session(
    self_signed_cert: tuple[bytes, bytes],
    cert_hash: bytes,
) -> None:
    """Each sequential client opens several bidi streams before disconnecting."""
    cert, key = self_signed_cert

    async with _echo_server(cert, key) as (_, port):
        for i in range(3):
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash],
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                for j in range(4):
                    send, recv = await session.open_bi()
                    payload = f"session-{i}-stream-{j}".encode()
                    await send.write(payload)
                    await send.finish()
                    data = await recv.read()
                    assert data == payload
                session.close()


@pytest.mark.asyncio
async def test_mixed_stream_types_across_sessions(
    self_signed_cert: tuple[bytes, bytes],
    cert_hash: bytes,
) -> None:
    """Sequential sessions alternate between bidi, uni, and datagram operations."""
    cert, key = self_signed_cert

    async with _echo_server(cert, key) as (_, port):
        # Session 1: bidi stream
        await _assert_bidi_echo(port, cert_hash, b"bidi-msg")

        # Session 2: unidirectional stream
        async with web_transport.Client(
            server_certificate_hashes=[cert_hash],
        ) as client:
            session = await client.connect(f"https://[::1]:{port}")
            send = await session.open_uni()
            await send.write(b"uni-msg")
            await send.finish()
            recv = await session.accept_uni()
            assert await recv.read() == b"uni-msg"
            session.close()

        # Session 3: datagram
        async with web_transport.Client(
            server_certificate_hashes=[cert_hash],
        ) as client:
            session = await client.connect(f"https://[::1]:{port}")
            session.send_datagram(b"dgram-msg")
            assert await session.receive_datagram() == b"dgram-msg"
            session.close()

        # Session 4: bidi again — confirm no cross-session corruption.
        await _assert_bidi_echo(port, cert_hash, b"bidi-again")


# ---------------------------------------------------------------------------
# Misbehaving client tests
#
# Each test: one client misbehaves, then a fresh well-behaved client
# connects and verifies full bidi echo. The server must survive every
# misbehavior without leaking state to subsequent connections.
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_after_client_resets_send_stream(
    self_signed_cert: tuple[bytes, bytes],
    cert_hash: bytes,
) -> None:
    """Client opens a bidi stream and resets its send side."""
    cert, key = self_signed_cert

    async with _echo_server(cert, key) as (_, port):
        async with web_transport.Client(
            server_certificate_hashes=[cert_hash],
        ) as client:
            session = await client.connect(f"https://[::1]:{port}")
            send, _recv = await session.open_bi()
            await send.write(b"will be reset")
            send.reset(42)
            session.close()

        await _assert_bidi_echo(port, cert_hash)


@pytest.mark.asyncio
async def test_after_client_stops_recv_stream(
    self_signed_cert: tuple[bytes, bytes],
    cert_hash: bytes,
) -> None:
    """Client opens a bidi stream, sends data, then stops the recv side.

    The server reads the data and tries to echo it back, but the client
    has told the server to stop sending via STOP_SENDING.
    """
    cert, key = self_signed_cert

    async with _echo_server(cert, key) as (_, port):
        async with web_transport.Client(
            server_certificate_hashes=[cert_hash],
        ) as client:
            session = await client.connect(f"https://[::1]:{port}")
            send, recv = await session.open_bi()
            await send.write(b"you won't read this back")
            await send.finish()
            recv.stop(77)
            session.close()

        await _assert_bidi_echo(port, cert_hash)


@pytest.mark.asyncio
async def test_after_client_resets_and_stops_same_stream(
    self_signed_cert: tuple[bytes, bytes],
    cert_hash: bytes,
) -> None:
    """Client resets send and stops recv on the same bidi stream."""
    cert, key = self_signed_cert

    async with _echo_server(cert, key) as (_, port):
        async with web_transport.Client(
            server_certificate_hashes=[cert_hash],
        ) as client:
            session = await client.connect(f"https://[::1]:{port}")
            send, recv = await session.open_bi()
            send.reset(1)
            recv.stop(2)
            session.close()

        await _assert_bidi_echo(port, cert_hash)


@pytest.mark.asyncio
async def test_after_client_abandons_open_stream(
    self_signed_cert: tuple[bytes, bytes],
    cert_hash: bytes,
) -> None:
    """Client opens a bidi stream, never writes, and closes the session."""
    cert, key = self_signed_cert

    async with _echo_server(cert, key) as (_, port):
        async with web_transport.Client(
            server_certificate_hashes=[cert_hash],
        ) as client:
            session = await client.connect(f"https://[::1]:{port}")
            await session.open_bi()  # opened but never used
            session.close()

        await _assert_bidi_echo(port, cert_hash)


@pytest.mark.asyncio
async def test_after_client_abandons_multiple_streams(
    self_signed_cert: tuple[bytes, bytes],
    cert_hash: bytes,
) -> None:
    """Client opens several bidi streams, resets some, stops others, then closes."""
    cert, key = self_signed_cert

    async with _echo_server(cert, key) as (_, port):
        async with web_transport.Client(
            server_certificate_hashes=[cert_hash],
        ) as client:
            session = await client.connect(f"https://[::1]:{port}")

            # Stream 1: write + reset
            s1_send, s1_recv = await session.open_bi()
            await s1_send.write(b"partial")
            s1_send.reset(10)
            s1_recv.stop(10)

            # Stream 2: open but never touch
            await session.open_bi()

            # Stream 3: finish send side, stop recv
            s3_send, s3_recv = await session.open_bi()
            await s3_send.write(b"complete")
            await s3_send.finish()
            s3_recv.stop(20)

            session.close()

        await _assert_bidi_echo(port, cert_hash)


@pytest.mark.asyncio
async def test_after_client_closes_session_with_error(
    self_signed_cert: tuple[bytes, bytes],
    cert_hash: bytes,
) -> None:
    """Client closes the session with a non-zero error code and reason."""
    cert, key = self_signed_cert

    async with _echo_server(cert, key) as (_, port):
        async with web_transport.Client(
            server_certificate_hashes=[cert_hash],
        ) as client:
            session = await client.connect(f"https://[::1]:{port}")
            send, _recv = await session.open_bi()
            await send.write(b"data before error close")
            session.close(code=999, reason="client error")

        await _assert_bidi_echo(port, cert_hash)


@pytest.mark.asyncio
async def test_after_client_closes_endpoint_without_session(
    self_signed_cert: tuple[bytes, bytes],
    cert_hash: bytes,
) -> None:
    """Client closes its QUIC endpoint directly, without closing the session first."""
    cert, key = self_signed_cert

    async with _echo_server(cert, key) as (_, port):
        client = web_transport.Client(server_certificate_hashes=[cert_hash])
        await client.__aenter__()
        session = await client.connect(f"https://[::1]:{port}")
        await session.open_bi()
        # Close the endpoint, not the session — forces a transport-level close.
        client.close()
        await client.__aexit__(None, None, None)

        await _assert_bidi_echo(port, cert_hash)


@pytest.mark.asyncio
async def test_after_client_writes_partial_then_closes(
    self_signed_cert: tuple[bytes, bytes],
    cert_hash: bytes,
) -> None:
    """Client writes data but never finishes the stream, then closes the session."""
    cert, key = self_signed_cert

    async with _echo_server(cert, key) as (_, port):
        async with web_transport.Client(
            server_certificate_hashes=[cert_hash],
        ) as client:
            session = await client.connect(f"https://[::1]:{port}")
            send, _recv = await session.open_bi()
            await send.write(b"no finish coming")
            # No finish(), no reset() — just close the session.
            session.close()

        await _assert_bidi_echo(port, cert_hash)


@pytest.mark.asyncio
async def test_after_idle_session(
    self_signed_cert: tuple[bytes, bytes],
    cert_hash: bytes,
) -> None:
    """Client connects, opens nothing, and closes immediately."""
    cert, key = self_signed_cert

    async with _echo_server(cert, key) as (_, port):
        async with web_transport.Client(
            server_certificate_hashes=[cert_hash],
        ) as client:
            session = await client.connect(f"https://[::1]:{port}")
            session.close()

        await _assert_bidi_echo(port, cert_hash)


@pytest.mark.asyncio
async def test_after_client_resets_uni_send_stream(
    self_signed_cert: tuple[bytes, bytes],
    cert_hash: bytes,
) -> None:
    """Client opens a uni stream, writes, then resets it."""
    cert, key = self_signed_cert

    async with _echo_server(cert, key) as (_, port):
        async with web_transport.Client(
            server_certificate_hashes=[cert_hash],
        ) as client:
            session = await client.connect(f"https://[::1]:{port}")
            send = await session.open_uni()
            await send.write(b"uni reset")
            send.reset(55)
            session.close()

        await _assert_bidi_echo(port, cert_hash)


@pytest.mark.asyncio
async def test_after_many_sequential_misbehaviors(
    self_signed_cert: tuple[bytes, bytes],
    cert_hash: bytes,
) -> None:
    """Chain multiple different misbehaviors back-to-back, then verify echo.

    Exercises cumulative server resilience — any leaked state from one
    bad session could compound with the next.
    """
    cert, key = self_signed_cert

    async with _echo_server(cert, key) as (_, port):
        # 1. Idle session
        async with web_transport.Client(
            server_certificate_hashes=[cert_hash],
        ) as client:
            session = await client.connect(f"https://[::1]:{port}")
            session.close()

        # 2. Stream reset
        async with web_transport.Client(
            server_certificate_hashes=[cert_hash],
        ) as client:
            session = await client.connect(f"https://[::1]:{port}")
            send, _recv = await session.open_bi()
            send.reset(1)
            session.close()

        # 3. Recv stop
        async with web_transport.Client(
            server_certificate_hashes=[cert_hash],
        ) as client:
            session = await client.connect(f"https://[::1]:{port}")
            send, recv = await session.open_bi()
            await send.write(b"data")
            await send.finish()
            recv.stop(2)
            session.close()

        # 4. Error code close
        async with web_transport.Client(
            server_certificate_hashes=[cert_hash],
        ) as client:
            session = await client.connect(f"https://[::1]:{port}")
            session.close(code=500, reason="intentional")

        # 5. Endpoint close
        client = web_transport.Client(server_certificate_hashes=[cert_hash])
        await client.__aenter__()
        session = await client.connect(f"https://[::1]:{port}")
        client.close()
        await client.__aexit__(None, None, None)

        # 6. Multiple abandoned streams
        async with web_transport.Client(
            server_certificate_hashes=[cert_hash],
        ) as client:
            session = await client.connect(f"https://[::1]:{port}")
            for _ in range(5):
                s, r = await session.open_bi()
                s.reset(0)
                r.stop(0)
            session.close()

        # Finally: a well-behaved client must succeed.
        await _assert_bidi_echo(port, cert_hash, b"survived everything")
