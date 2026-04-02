"""Browser interop tests for SessionRequest handling."""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING, Any

import pytest

import web_transport
from .conftest import _webtransport_connect_js

if TYPE_CHECKING:
    from .conftest import RunJS, RunJSRaw, ServerFactory

pytestmark = pytest.mark.asyncio(loop_scope="session")


async def test_request_url_matches_target(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """request.url contains expected host and port."""
    async with start_server() as (server, port, hash_b64):
        request_url: str = ""

        async def server_side() -> None:
            nonlocal request_url
            request = await server.accept()
            assert request is not None
            request_url = request.url
            session = await request.accept()
            async with session:
                pass

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(port, hash_b64, "await transport.closed; return true;")

    assert "127.0.0.1" in request_url
    assert str(port) in request_url


async def test_request_url_with_path(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """Browser connects to https://host:port/custom/path → request.url includes path."""
    async with start_server() as (server, port, hash_b64):
        request_url: str = ""

        async def server_side() -> None:
            nonlocal request_url
            request = await server.accept()
            assert request is not None
            request_url = request.url
            session = await request.accept()
            async with session:
                await session.wait_closed()

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js_raw(f"""
                {setup}
                const pathUrl = "https://127.0.0.1:{port}/custom/path";
                const transport = new WebTransport(pathUrl, transportOptions);
                await transport.ready;
                transport.close();
                await transport.closed;
                return true;
            """)

    assert "/custom/path" in request_url


async def test_accept_then_reject_raises(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """request.accept() then request.reject() raises SessionError."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            try:
                await request.reject(404)
            except web_transport.SessionError as e:
                error = e
            async with session:
                session.close()

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                try {{
                    await transport.ready;
                }} catch (e) {{
                    // May fail if session was closed
                }}
                try {{ transport.close(); }} catch (e) {{}}
                return true;
            """)

    assert isinstance(error, web_transport.SessionError)


async def test_reject_then_accept_raises(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """request.reject() then request.accept() raises SessionError."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            await request.reject(404)
            try:
                await request.accept()
            except web_transport.SessionError as e:
                error = e

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                try {{
                    await transport.ready;
                }} catch (e) {{
                    // Expected: session was rejected
                }}
                return true;
            """)

    assert isinstance(error, web_transport.SessionError)


async def test_server_async_iterator(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """async for request in server: accepts browser connection."""
    async with start_server() as (server, port, hash_b64):
        received_request = False

        async def server_side() -> None:
            nonlocal received_request
            async for request in server:
                received_request = True
                session = await request.accept()
                async with session:
                    send, recv = await session.accept_bi()
                    async with send:
                        data = await recv.read()
                        await send.write(data)
                    await session.wait_closed()
                break
            server.close()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const stream = await transport.createBidirectionalStream();
                await writeAllString(stream.writable, "iter-test");
                return await readAllString(stream.readable);
            """,
            )

    assert received_request is True
    assert result == "iter-test"


async def test_reject_with_default_status(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """request.reject() with no arguments → browser sees rejection (default 404)."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            await request.reject()  # no args — default 404

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                try {{
                    await transport.ready;
                    return {{ rejected: false }};
                }} catch (e) {{
                    return {{ rejected: true, message: e.toString() }};
                }}
            """)

    assert isinstance(result, dict)
    assert result["rejected"] is True


async def test_request_url_with_query_params(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """Browser connects with ?key=value → request.url includes query string."""
    async with start_server() as (server, port, hash_b64):
        request_url: str = ""

        async def server_side() -> None:
            nonlocal request_url
            request = await server.accept()
            assert request is not None
            request_url = request.url
            session = await request.accept()
            async with session:
                await session.wait_closed()

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js_raw(f"""
                {setup}
                const pathUrl = "https://127.0.0.1:{port}/endpoint?key=value&foo=bar";
                const transport = new WebTransport(pathUrl, transportOptions);
                await transport.ready;
                transport.close();
                await transport.closed;
                return true;
            """)

    assert "key=value" in request_url
    assert "foo=bar" in request_url
