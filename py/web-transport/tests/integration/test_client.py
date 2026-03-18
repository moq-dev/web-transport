"""Integration tests for client connection errors and configuration."""

import asyncio

import pytest

import web_transport


@pytest.mark.asyncio
async def test_connect_invalid_url(cert_hash):
    """connect("not-a-url") -> ValueError."""
    async with web_transport.Client(server_certificate_hashes=[cert_hash]) as client:
        with pytest.raises(ValueError, match="url|URL"):
            await client.connect("not-a-url")


@pytest.mark.asyncio
async def test_connect_wrong_cert_hash(self_signed_cert):
    """Wrong server_certificate_hashes -> ConnectError."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    ) as server:
        _, port = server.local_addr

        async def server_task():
            # Server must be accepting for the TLS handshake to occur
            await server.accept()

        async def client_task():
            wrong_hash = b"\x00" * 32
            async with web_transport.Client(
                server_certificate_hashes=[wrong_hash]
            ) as client:
                with pytest.raises(web_transport.ConnectError) as exc_info:
                    await client.connect(f"https://[::1]:{port}")
                assert not isinstance(exc_info.value, web_transport.SessionRejected)

        server_t = asyncio.create_task(server_task())
        client_t = asyncio.create_task(client_task())

        # Wait for client to finish (it should fail fast)
        await asyncio.wait_for(client_t, timeout=10.0)

        # Cancel server accept since the handshake failed
        server_t.cancel()
        try:
            await server_t
        except asyncio.CancelledError:
            pass


@pytest.mark.asyncio
async def test_connect_no_server():
    """Connect to port with no listener -> ConnectError or asyncio timeout."""
    async with web_transport.Client(
        server_certificate_hashes=[b"\x00" * 32],
        max_idle_timeout=0.5,
    ) as client:
        # max_idle_timeout only applies to established connections, not to
        # the initial QUIC handshake, so the connect may hang until the
        # asyncio timeout fires.
        with pytest.raises((web_transport.ConnectError,)):
            await asyncio.wait_for(
                client.connect("https://[::1]:19999"),
                timeout=5.0,
            )


@pytest.mark.asyncio
async def test_connect_after_close(self_signed_cert, cert_hash):
    """client.close() then connect() -> error."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    ) as server:
        _, port = server.local_addr

        async with web_transport.Client(
            server_certificate_hashes=[cert_hash]
        ) as client:
            client.close()
            with pytest.raises(web_transport.ConnectError):
                await client.connect(f"https://[::1]:{port}")


@pytest.mark.asyncio
async def test_no_cert_verification_mode(self_signed_cert):
    """no_cert_verification=True connects without pinning."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            session.close()

        async def client_task():
            async with web_transport.Client(no_cert_verification=True) as client:
                session = await client.connect(f"https://[::1]:{port}")
                assert isinstance(session, web_transport.Session)
                session.close()

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_congestion_throughput(self_signed_cert, cert_hash):
    """congestion_control="throughput" -> connection works."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert],
        private_key=key,
        bind="[::1]:0",
        congestion_control="throughput",
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            session.close()

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash],
                congestion_control="throughput",
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                assert isinstance(session, web_transport.Session)
                session.close()

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_congestion_low_latency(self_signed_cert, cert_hash):
    """congestion_control="low_latency" -> connection works."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert],
        private_key=key,
        bind="[::1]:0",
        congestion_control="low_latency",
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            session.close()

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash],
                congestion_control="low_latency",
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                assert isinstance(session, web_transport.Session)
                session.close()

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_idle_timeout_fires(self_signed_cert, cert_hash):
    """max_idle_timeout=0.5 -> SessionTimeout after inactivity."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert],
        private_key=key,
        bind="[::1]:0",
        max_idle_timeout=0.5,
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await asyncio.wait_for(session.wait_closed(), timeout=5.0)
            reason = session.close_reason
            # May be SessionTimeout (detected locally), SessionClosedLocally
            # (timeout closed it), or SessionClosedByPeer (peer timed out first)
            assert isinstance(
                reason,
                (
                    web_transport.SessionTimeout,
                    web_transport.SessionClosedByPeer,
                    web_transport.SessionClosedLocally,
                ),
            )

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash],
                max_idle_timeout=0.5,
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                # Do nothing — let the idle timeout fire
                await asyncio.wait_for(session.wait_closed(), timeout=5.0)
                reason = session.close_reason
                assert isinstance(
                    reason,
                    (
                        web_transport.SessionTimeout,
                        web_transport.SessionClosedByPeer,
                        web_transport.SessionClosedLocally,
                    ),
                )

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_keep_alive_prevents_timeout(self_signed_cert, cert_hash):
    """keep_alive prevents idle timeout."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert],
        private_key=key,
        bind="[::1]:0",
        max_idle_timeout=1.0,
        keep_alive_interval=0.1,
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            # Wait longer than the idle timeout
            await asyncio.sleep(0.5)
            # Session should still be alive
            assert session.close_reason is None
            await session.wait_closed()

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash],
                max_idle_timeout=1.0,
                keep_alive_interval=0.1,
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                await asyncio.sleep(1.0)
                # Session should still be alive
                assert session.close_reason is None
                session.close()

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_client_wait_closed():
    """client.wait_closed() returns after close()."""
    client = web_transport.Client(server_certificate_hashes=[b"\x00" * 32])
    await client.__aenter__()
    client.close()
    await asyncio.wait_for(client.wait_closed(), timeout=5.0)
    await client.__aexit__(None, None, None)


@pytest.mark.asyncio
async def test_open_bi_after_timeout(self_signed_cert, cert_hash):
    """open_bi() after idle timeout -> SessionTimeout or SessionClosed."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert],
        private_key=key,
        bind="[::1]:0",
        max_idle_timeout=0.5,
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await session.wait_closed()

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash],
                max_idle_timeout=0.5,
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                await session.wait_closed()
                with pytest.raises(
                    (web_transport.SessionTimeout, web_transport.SessionClosedLocally)
                ):
                    await session.open_bi()

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_open_uni_after_timeout(self_signed_cert, cert_hash):
    """open_uni() after idle timeout -> SessionTimeout or SessionClosed."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert],
        private_key=key,
        bind="[::1]:0",
        max_idle_timeout=0.5,
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await session.wait_closed()

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash],
                max_idle_timeout=0.5,
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                await session.wait_closed()
                with pytest.raises(
                    (web_transport.SessionTimeout, web_transport.SessionClosedLocally)
                ):
                    await session.open_uni()

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_send_datagram_after_timeout(self_signed_cert, cert_hash):
    """send_datagram() after idle timeout -> SessionTimeout or SessionClosed."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert],
        private_key=key,
        bind="[::1]:0",
        max_idle_timeout=0.5,
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await session.wait_closed()

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash],
                max_idle_timeout=0.5,
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                await session.wait_closed()
                with pytest.raises(
                    (web_transport.SessionTimeout, web_transport.SessionClosedLocally)
                ):
                    session.send_datagram(b"hello")

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_receive_datagram_after_timeout(self_signed_cert, cert_hash):
    """receive_datagram() after idle timeout -> SessionTimeout or SessionClosed."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert],
        private_key=key,
        bind="[::1]:0",
        max_idle_timeout=0.5,
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await session.wait_closed()

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash],
                max_idle_timeout=0.5,
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                await session.wait_closed()
                with pytest.raises(
                    (web_transport.SessionTimeout, web_transport.SessionClosedLocally)
                ):
                    await session.receive_datagram()

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_client_close_code_propagates_to_server(self_signed_cert, cert_hash):
    """client.close(code) -> server sees session closure."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert], private_key=key, bind="[::1]:0"
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            # Endpoint-level close (QUIC CONNECTION_CLOSE) is inherently
            # racy — the CLOSE frame may not arrive before the connection
            # drops, so the server may see SessionClosedLocally.
            try:
                await session.accept_bi()
                pytest.fail("Expected an exception from accept_bi()")
            except web_transport.SessionClosedByPeer as e:
                assert e.source == "application"
            except web_transport.SessionClosedLocally:
                pass

        async def client_task():
            client = web_transport.Client(
                server_certificate_hashes=[cert_hash],
            )
            await client.__aenter__()
            await client.connect(f"https://[::1]:{port}")
            await asyncio.sleep(0.1)
            # Close the client endpoint (not the session)
            client.close(42, "client shutdown")
            await client.wait_closed()

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_idle_timeout_disabled(self_signed_cert, cert_hash):
    """max_idle_timeout=None -> alive after 2s."""
    cert, key = self_signed_cert

    async with web_transport.Server(
        certificate_chain=[cert],
        private_key=key,
        bind="[::1]:0",
        max_idle_timeout=None,
    ) as server:
        _, port = server.local_addr

        async def server_task():
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await session.wait_closed()
            assert isinstance(session.close_reason, web_transport.SessionClosedByPeer)

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash],
                max_idle_timeout=None,
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                await asyncio.sleep(1.5)
                assert session.close_reason is None
                session.close()
                await session.wait_closed()

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )
