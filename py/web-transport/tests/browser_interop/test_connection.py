"""Browser interop tests for WebTransport connection lifecycle."""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING, Any

import pytest

import web_transport
from .conftest import _webtransport_connect_js

if TYPE_CHECKING:
    from .conftest import RunJS, RunJSRaw, ServerFactory

pytestmark = pytest.mark.asyncio(loop_scope="session")


async def test_browser_connects_and_ready_resolves(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Chromium establishes a WebTransport session and transport.ready resolves."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                """
                const api = {
                    createBidirectionalStream: typeof transport.createBidirectionalStream === "function",
                    createUnidirectionalStream: typeof transport.createUnidirectionalStream === "function",
                    datagrams: transport.datagrams !== undefined,
                    close: typeof transport.close === "function",
                    ready: transport.ready instanceof Promise,
                    closed: transport.closed instanceof Promise,
                };
                const missing = Object.entries(api)
                    .filter(([, v]) => !v).map(([k]) => k);
                if (missing.length > 0)
                    throw new Error("Missing API: " + missing.join(", "));
                return true;
            """,
            )

    assert result is True


async def test_browser_close_with_code_and_reason(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """Browser close(code, reason) is observed by the server."""
    async with start_server() as (server, port, hash_b64):
        close_reason: web_transport.SessionError | None = None

        async def server_side() -> None:
            nonlocal close_reason
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await session.wait_closed()
            close_reason = session.close_reason

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                await transport.ready;
                transport.close({{closeCode: 7, reason: "done"}});
                await transport.closed;
                return true;
            """)

    assert isinstance(close_reason, web_transport.SessionClosedByPeer)
    assert close_reason.source == "session"
    assert close_reason.code == 7
    assert close_reason.reason == "done"


async def test_browser_close_default_code(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """Browser close() with no arguments yields code=0 and empty reason."""
    async with start_server() as (server, port, hash_b64):
        close_reason: web_transport.SessionError | None = None

        async def server_side() -> None:
            nonlocal close_reason
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await session.wait_closed()
            close_reason = session.close_reason

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                await transport.ready;
                transport.close();
                await transport.closed;
                return true;
            """)

    assert isinstance(close_reason, web_transport.SessionClosedByPeer)
    assert close_reason.source == "session"
    assert close_reason.code == 0
    assert close_reason.reason == ""


async def test_server_close_with_code_and_reason(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """Server session.close(code, reason) is observed by the browser."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                session.close(42, "bye")

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                await transport.ready;
                const closed = await transport.closed;
                return {{
                    closeCode: closed.closeCode,
                    reason: closed.reason,
                }};
            """)

    assert isinstance(result, dict)
    assert result["closeCode"] == 42
    assert result["reason"] == "bye"


async def test_server_close_default_code(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """Server session.close() with defaults is observed by the browser."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                session.close()

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                await transport.ready;
                const closed = await transport.closed;
                return {{
                    closeCode: closed.closeCode,
                    reason: closed.reason,
                }};
            """)

    assert isinstance(result, dict)
    assert result["closeCode"] == 0
    assert result["reason"] == ""


async def test_session_request_url(start_server: ServerFactory, run_js: RunJS) -> None:
    """Server inspects request.url and verifies it matches the expected address."""
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

    assert f"127.0.0.1:{port}" in request_url


async def test_session_rejection_404(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """Server reject(404) causes browser transport.ready to reject."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            await request.reject(404)

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


async def test_session_rejection_403(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """Server reject(403) causes browser transport.ready to reject."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            await request.reject(403)

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


async def test_server_accept_returns_none_after_close(
    start_server: ServerFactory,
) -> None:
    """server.close() causes server.accept() to return None."""
    async with start_server() as (server, _port, _hash_b64):
        server.close()
        result = await asyncio.wait_for(server.accept(), timeout=5)
        assert result is None


async def test_server_local_addr(start_server: ServerFactory) -> None:
    """server.local_addr returns a valid (host, port) tuple."""
    async with start_server() as (server, port, _hash_b64):
        host, p = server.local_addr
        assert host == "127.0.0.1"
        assert p == port
        assert p > 0


async def test_session_remote_address(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """session.remote_address returns the browser's address."""
    async with start_server() as (server, port, hash_b64):
        remote: tuple[str, int] = ("", 0)

        async def server_side() -> None:
            nonlocal remote
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                remote = session.remote_address

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(port, hash_b64, "await transport.closed; return true;")

    assert remote[0] == "127.0.0.1"
    assert remote[1] > 0


async def test_session_rtt_positive(start_server: ServerFactory, run_js: RunJS) -> None:
    """session.rtt is positive on loopback."""
    async with start_server() as (server, port, hash_b64):
        rtt: float = 0.0

        async def server_side() -> None:
            nonlocal rtt
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                rtt = session.rtt

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(port, hash_b64, "await transport.closed; return true;")

    assert rtt > 0


async def test_session_max_datagram_size_positive(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """session.max_datagram_size is positive."""
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


async def test_session_close_reason_none_while_open(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """session.close_reason is None before the session is closed."""
    async with start_server() as (server, port, hash_b64):
        reason_while_open: object = "sentinel"

        async def server_side() -> None:
            nonlocal reason_while_open
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                reason_while_open = session.close_reason

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(port, hash_b64, "await transport.closed; return true;")

    assert reason_while_open is None


async def test_session_close_reason_after_browser_close(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """After browser closes, wait_closed() returns and close_reason is set."""
    async with start_server() as (server, port, hash_b64):
        close_reason: web_transport.SessionError | None = None

        async def server_side() -> None:
            nonlocal close_reason
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await session.wait_closed()
            close_reason = session.close_reason

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                transport.close({closeCode: 99, reason: "test"});
                return true;
            """,
            )

    # The server sees a SessionClosed (either ByPeer or Locally depending
    # on the race between QUIC close processing and server teardown).
    assert isinstance(close_reason, web_transport.SessionClosed)


async def test_session_close_is_idempotent(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Calling session.close() twice does not raise."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                session.close()
                session.close()  # Should not raise

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                await transport.closed;
                return true;
            """,
            )


async def test_session_context_manager_closes(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """async with session: clean exit closes the session → browser sees it."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                pass  # clean exit should close the session

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                await transport.ready;
                try {{
                    const closed = await transport.closed;
                    return {{ closed: true, closeCode: closed.closeCode }};
                }} catch (e) {{
                    return {{ closed: true, error: e.toString() }};
                }}
            """)

    assert isinstance(result, dict)
    assert result["closed"] is True


async def test_session_context_manager_closes_on_exception(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """Exception inside async with session: still closes the session."""
    async with start_server() as (server, port, hash_b64):

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            try:
                async with session:
                    raise RuntimeError("intentional")
            except RuntimeError:
                pass

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                await transport.ready;
                try {{
                    await transport.closed;
                    return {{ closed: true }};
                }} catch (e) {{
                    return {{ closed: true, error: e.toString() }};
                }}
            """)

    assert isinstance(result, dict)
    assert result["closed"] is True


async def test_session_close_max_code(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """Browser closes with code 2^32-1 → server sees the max u32 code."""
    max_code = 2**32 - 1
    async with start_server() as (server, port, hash_b64):
        close_reason: web_transport.SessionError | None = None

        async def server_side() -> None:
            nonlocal close_reason
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await session.wait_closed()
            close_reason = session.close_reason

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                await transport.ready;
                transport.close({{closeCode: {max_code}, reason: "max"}});
                await transport.closed;
                return true;
            """)

    assert isinstance(close_reason, web_transport.SessionClosedByPeer)
    assert close_reason.code == max_code
    assert close_reason.reason == "max"


async def test_session_wait_closed_blocks_until_browser_closes(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """wait_closed() does not return until the browser calls transport.close()."""
    async with start_server() as (server, port, hash_b64):
        wait_closed_done = asyncio.Event()

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await session.wait_closed()
            wait_closed_done.set()

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result: Any = await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                await transport.ready;
                // Wait a bit to prove wait_closed blocks
                await new Promise(r => setTimeout(r, 300));
                transport.close({{closeCode: 0, reason: ""}});
                await transport.closed;
                return true;
            """)

    assert result is True
    assert wait_closed_done.is_set()


async def test_open_uni_after_close_raises(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """session.close() then open_uni() raises SessionClosedLocally."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                session.close()
                await session.wait_closed()
                try:
                    await session.open_uni()
                except web_transport.SessionClosedLocally as e:
                    error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            try:
                await run_js(port, hash_b64, "await transport.closed; return true;")
            except Exception:
                pass

    assert isinstance(error, web_transport.SessionClosedLocally)


async def test_accept_bi_after_local_close_raises(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """session.close() then accept_bi() raises SessionClosedLocally."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:
                session.close()
                await session.wait_closed()
                try:
                    await session.accept_bi()
                except web_transport.SessionClosedLocally as e:
                    error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            try:
                await run_js(port, hash_b64, "await transport.closed; return true;")
            except Exception:
                pass

    assert isinstance(error, web_transport.SessionClosedLocally)


async def test_accept_uni_after_browser_close_raises(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """Browser closes → pending accept_uni() raises SessionClosed."""
    async with start_server() as (server, port, hash_b64):
        error: BaseException | None = None

        async def server_side() -> None:
            nonlocal error
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            try:
                await session.accept_uni()
            except web_transport.SessionClosed as e:
                error = e

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                """
                transport.close({closeCode: 0, reason: ""});
                return true;
            """,
            )

    assert isinstance(error, web_transport.SessionClosed)


async def test_close_unicode_reason(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """Browser closes with Unicode reason containing emoji, server reads it correctly."""
    async with start_server() as (server, port, hash_b64):
        close_reason: web_transport.SessionError | None = None

        async def server_side() -> None:
            nonlocal close_reason
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await session.wait_closed()
            close_reason = session.close_reason

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                await transport.ready;
                transport.close({{closeCode: 1, reason: "goodbye \ud83d\udc4b\ud83c\udf0d"}});
                await transport.closed;
                return true;
            """)

    assert isinstance(close_reason, web_transport.SessionClosedByPeer)
    assert close_reason.code == 1
    assert close_reason.reason == "goodbye \U0001f44b\U0001f30d"


async def test_close_long_reason(
    start_server: ServerFactory, run_js_raw: RunJSRaw
) -> None:
    """Browser closes with 1KB reason string, server reads it without truncation."""
    long_reason = "x" * 1024
    async with start_server() as (server, port, hash_b64):
        close_reason: web_transport.SessionError | None = None

        async def server_side() -> None:
            nonlocal close_reason
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            await session.wait_closed()
            close_reason = session.close_reason

        setup = _webtransport_connect_js(port, hash_b64)
        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js_raw(f"""
                {setup}
                const transport = new WebTransport(url, transportOptions);
                await transport.ready;
                transport.close({{closeCode: 1, reason: "x".repeat(1024)}});
                await transport.closed;
                return true;
            """)

    assert isinstance(close_reason, web_transport.SessionClosedByPeer)
    assert close_reason.code == 1
    assert close_reason.reason == long_reason
