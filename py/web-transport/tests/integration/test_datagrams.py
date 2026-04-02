"""Integration tests for datagram functionality."""

import asyncio

import pytest

import web_transport


@pytest.mark.asyncio
async def test_datagram_roundtrip(self_signed_cert, cert_hash):
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert],
        private_key=key,
        bind="[::1]:0",
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                data = await session.receive_datagram()
                assert data == b"ping"
                session.send_datagram(data)

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash],
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                async with session:
                    session.send_datagram(b"ping")
                    response = await session.receive_datagram()
                    assert response == b"ping"

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_max_datagram_size(self_signed_cert, cert_hash):
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert],
        private_key=key,
        bind="[::1]:0",
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                size = session.max_datagram_size
                assert isinstance(size, int)
                # QUIC guarantees ~1200 byte minimum MTU
                assert size >= 1000, f"max_datagram_size unexpectedly small: {size}"

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash],
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                async with session:
                    size = session.max_datagram_size
                    assert isinstance(size, int)
                    assert size >= 1000, f"max_datagram_size unexpectedly small: {size}"

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_datagram_too_large(session_pair):
    """Send datagram > max_datagram_size -> DatagramTooLargeError."""
    _server_session, client_session = session_pair

    max_size = client_session.max_datagram_size

    # Exact max_size should succeed (boundary value)
    client_session.send_datagram(b"x" * max_size)

    # max_size + 1 should fail
    oversized = b"x" * (max_size + 1)
    with pytest.raises(web_transport.DatagramTooLargeError):
        client_session.send_datagram(oversized)


@pytest.mark.asyncio
async def test_empty_datagram(session_pair):
    """send_datagram(b"") succeeds."""
    server_session, client_session = session_pair

    async def server_side():
        data = await server_session.receive_datagram()
        assert data == b""

    task = asyncio.create_task(server_side())
    client_session.send_datagram(b"")
    await asyncio.wait_for(task, timeout=5.0)


@pytest.mark.asyncio
async def test_multiple_datagrams(session_pair):
    """Send N datagrams rapidly -> all received on loopback."""
    server_session, client_session = session_pair

    n = 20
    received = []

    async def server_side():
        for _ in range(n):
            data = await server_session.receive_datagram()
            received.append(data)

    task = asyncio.create_task(server_side())

    for i in range(n):
        client_session.send_datagram(f"msg-{i}".encode())

    await asyncio.wait_for(task, timeout=5.0)
    assert len(received) == n
    expected = {f"msg-{i}".encode() for i in range(n)}
    assert set(received) == expected


@pytest.mark.asyncio
async def test_datagram_binary_data(session_pair):
    """All byte values 0x00-0xFF roundtrip intact."""
    server_session, client_session = session_pair

    payload = bytes(range(256))

    async def server_side():
        data = await server_session.receive_datagram()
        assert data == payload
        server_session.send_datagram(data)

    task = asyncio.create_task(server_side())
    client_session.send_datagram(payload)
    response = await asyncio.wait_for(client_session.receive_datagram(), timeout=5.0)
    assert response == payload
    await task
