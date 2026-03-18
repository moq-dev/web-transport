import asyncio
from contextlib import asynccontextmanager

import pytest_asyncio

import web_transport


@pytest_asyncio.fixture
async def session_pair(self_signed_cert, cert_hash):
    """Create a connected server/client session pair."""
    cert, key = self_signed_cert

    server = web_transport.Server(
        certificate_chain=[cert],
        private_key=key,
        bind="[::1]:0",
    )
    await server.__aenter__()
    _, port = server.local_addr

    client = web_transport.Client(server_certificate_hashes=[cert_hash])
    await client.__aenter__()

    # Connect
    async def accept():
        request = await server.accept()
        assert request is not None
        return await request.accept()

    server_session, client_session = await asyncio.gather(
        accept(),
        client.connect(f"https://[::1]:{port}"),
    )

    yield server_session, client_session

    client_session.close()
    server_session.close()
    await client_session.wait_closed()
    await server_session.wait_closed()
    await client.__aexit__(None, None, None)
    await server.__aexit__(None, None, None)


@asynccontextmanager
async def connect_pair(cert, key, cert_hash, *, server_kwargs=None, client_kwargs=None):
    """Create a connected session pair with custom server/client config.

    Yields (server, client, server_session, client_session).
    """
    server_kw = {"certificate_chain": [cert], "private_key": key, "bind": "[::1]:0"}
    if server_kwargs:
        server_kw.update(server_kwargs)

    client_kw = {"server_certificate_hashes": [cert_hash]}
    if client_kwargs:
        client_kw.update(client_kwargs)

    server = web_transport.Server(**server_kw)  # type: ignore[invalid-argument-type]
    await server.__aenter__()
    _, port = server.local_addr

    client = web_transport.Client(**client_kw)  # type: ignore[invalid-argument-type]
    await client.__aenter__()

    async def accept():
        request = await server.accept()
        assert request is not None
        return await request.accept()

    server_session, client_session = await asyncio.gather(
        accept(),
        client.connect(f"https://[::1]:{port}"),
    )

    try:
        yield server, client, server_session, client_session
    finally:
        client_session.close()
        server_session.close()
        await client_session.wait_closed()
        await server_session.wait_closed()
        await client.__aexit__(None, None, None)
        await server.__aexit__(None, None, None)
