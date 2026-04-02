import asyncio

import pytest

import web_transport


@pytest.mark.asyncio
async def test_echo_bidirectional(self_signed_cert, cert_hash):
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
            assert "localhost" in request.url or f"[::1]:{port}" in request.url
            session = await request.accept()
            async with session:
                send, recv = await session.accept_bi()
                async with send:
                    data = await recv.read()
                    await send.write(data)

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash],
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                async with session:
                    send, recv = await session.open_bi()
                    async with send:
                        await send.write(b"Hello, WebTransport!")
                    response = await recv.read()
                    assert response == b"Hello, WebTransport!"

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )


@pytest.mark.asyncio
async def test_echo_unidirectional(self_signed_cert, cert_hash):
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
            assert "localhost" in request.url or f"[::1]:{port}" in request.url
            session = await request.accept()
            async with session:
                recv = await session.accept_uni()
                data = await recv.read()
                send = await session.open_uni()
                async with send:
                    await send.write(data)

        async def client_task():
            async with web_transport.Client(
                server_certificate_hashes=[cert_hash],
            ) as client:
                session = await client.connect(f"https://[::1]:{port}")
                async with session:
                    send = await session.open_uni()
                    async with send:
                        await send.write(b"uni echo")
                    recv = await session.accept_uni()
                    response = await recv.read()
                    assert response == b"uni echo"

        await asyncio.gather(
            asyncio.create_task(server_task()),
            asyncio.create_task(client_task()),
        )
