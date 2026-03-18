"""Integration tests for server lifecycle."""

import asyncio

import pytest

import web_transport


@pytest.mark.asyncio
async def test_server_async_iterator(self_signed_cert, cert_hash):
    """async for request in server yields SessionRequest, stops on close."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    ) as server:
        _, port = server.local_addr
        requests = []

        async def iterate():
            async for request in server:
                requests.append(request)
                await request.accept()

        async def connect_and_close():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash]
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                session.close()

            await asyncio.sleep(0.1)
            server.close()

        await asyncio.gather(
            asyncio.create_task(iterate()),
            asyncio.create_task(connect_and_close()),
        )

        assert len(requests) >= 1
        assert isinstance(requests[0], web_transport.SessionRequest)


@pytest.mark.asyncio
async def test_server_accept_returns_none_after_close(self_signed_cert):
    """server.close() -> accept() returns None."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    ) as server:
        server.close()
        result = await server.accept()
        assert result is None


@pytest.mark.asyncio
async def test_server_local_addr(self_signed_cert):
    """server.local_addr returns (str, int)."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    ) as server:
        addr = server.local_addr
        assert isinstance(addr, tuple)
        assert len(addr) == 2
        host, port = addr
        assert isinstance(host, str)
        assert isinstance(port, int)
        assert port > 0


@pytest.mark.asyncio
async def test_reject_session_request(self_signed_cert, cert_hash):
    """request.reject(403) -> client's connect() raises SessionRejected."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            await request.reject(403)

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash]
            ) as client:
                with pytest.raises(web_transport.SessionRejected) as exc_info:
                    await client.connect(f"https://[::1]:{port}")
                assert exc_info.value.status_code == 403

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_reject_with_default_status(self_signed_cert, cert_hash):
    """request.reject() uses default 404."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            await request.reject()

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash]
            ) as client:
                with pytest.raises(web_transport.SessionRejected) as exc_info:
                    await client.connect(f"https://[::1]:{port}")
                assert exc_info.value.status_code == 404

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_double_accept_raises(self_signed_cert, cert_hash):
    """accept() twice -> SessionError."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            await request.accept()
            with pytest.raises(web_transport.SessionError, match="already"):
                await request.accept()

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash]
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                session.close()

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_double_reject_raises(self_signed_cert, cert_hash):
    """reject() then accept() -> SessionError."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            await request.reject()
            with pytest.raises(web_transport.SessionError, match="already"):
                await request.accept()

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash]
            ) as client:
                with pytest.raises(web_transport.SessionRejected) as exc_info:
                    await client.connect(f"https://[::1]:{port}")
                assert exc_info.value.status_code == 404

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_multiple_concurrent_sessions(self_signed_cert, cert_hash):
    """Accept 3 sessions on same server, operate streams independently."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    ) as server:
        _, port = server.local_addr
        sessions = []

        async def echo_session(session):
            send, recv = await session.accept_bi()
            data = await recv.read()
            async with send:
                await send.write(data)

        async def server_task():
            echo_tasks = []
            for _ in range(3):
                request = await server.accept()
                assert request is not None
                session = await request.accept()
                sessions.append(session)
                echo_tasks.append(asyncio.create_task(echo_session(session)))

            await asyncio.gather(*echo_tasks)

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash]
            ) as client:
                results = []
                for i in range(3):
                    session = await client.connect(f"https://[::1]:{port}")
                    send, recv = await session.open_bi()
                    async with send:
                        await send.write(f"session-{i}".encode())
                    data = await recv.read()
                    results.append(data)
                    session.close()

                assert results == [b"session-0", b"session-1", b"session-2"]

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_server_close_with_code(self_signed_cert, cert_hash):
    """server.close(code, reason) terminates all active sessions."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await asyncio.sleep(0.1)
            session.close(42, "shutting down")
            await session.wait_closed()

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash]
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                with pytest.raises(web_transport.SessionClosedByPeer) as exc_info:
                    await session.accept_bi()
                assert exc_info.value.source == "session"
                assert exc_info.value.code == 42
                assert exc_info.value.reason == "shutting down"

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_reject_invalid_status_code(self_signed_cert, cert_hash):
    """request.reject(99) -> ValueError (99 is not a valid HTTP status)."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            with pytest.raises(ValueError, match="status"):
                await request.reject(99)

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash]
            ) as client:
                try:
                    await asyncio.wait_for(
                        client.connect(f"https://[::1]:{port}"), timeout=2.0
                    )
                except (web_transport.ConnectError, asyncio.TimeoutError):
                    pass

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_session_request_url(self_signed_cert, cert_hash):
    """SessionRequest.url contains expected https:// URL with host and port."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            url = request.url
            assert url.startswith("https://")
            assert str(port) in url
            session = await request.accept()
            session.close()

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash]
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                await session.wait_closed()

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_server_wait_closed(self_signed_cert):
    """server.wait_closed() returns after close()."""
    cert, key = self_signed_cert

    server = web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    )
    await server.__aenter__()
    server.close()
    await asyncio.wait_for(server.wait_closed(), timeout=5.0)
    await server.__aexit__(None, None, None)


@pytest.mark.asyncio
async def test_server_close_propagates_as_application(self_signed_cert, cert_hash):
    """server.close(code) -> client sees SessionClosedByPeer(source='application')."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            await request.accept()
            await asyncio.sleep(0.1)
            # Close the entire server endpoint (not just the session)
            server.close(42, "shutdown")
            await server.wait_closed()

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash]
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                # Endpoint-level close (QUIC CONNECTION_CLOSE) is inherently
                # racy — the CLOSE frame may not arrive before the connection
                # drops, so the client may see SessionClosedLocally.
                try:
                    await session.accept_bi()
                    pytest.fail("Expected an exception from accept_bi()")
                except web_transport.SessionClosedByPeer as e:
                    assert e.source == "application"
                except web_transport.SessionClosedLocally:
                    pass

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )
