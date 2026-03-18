"""Fixtures and helpers for browser interop tests.

Uses Playwright's async API to drive headless Chromium against the Python
WebTransport server.  All async fixtures and tests share a session-scoped
event loop so that the single Chromium instance can be reused across tests.
"""

from __future__ import annotations

import base64
from collections.abc import AsyncIterator, Awaitable, Callable
from contextlib import AbstractAsyncContextManager, asynccontextmanager
from typing import Any

import pytest
import pytest_asyncio
from playwright.async_api import Browser, Page, Route, async_playwright

import web_transport

# ---------------------------------------------------------------------------
# Event loop configuration — session-scoped for all tests in this directory
# ---------------------------------------------------------------------------

pytestmark = pytest.mark.asyncio(loop_scope="session")

# ---------------------------------------------------------------------------
# Type aliases
# ---------------------------------------------------------------------------

ServerContext = AbstractAsyncContextManager[tuple[web_transport.Server, int, str]]
ServerFactory = Callable[..., ServerContext]
RunJS = Callable[[int, str, str], Awaitable[Any]]
RunJSRaw = Callable[[str], Awaitable[Any]]

# ---------------------------------------------------------------------------
# Reusable JS helpers injected into every run_js call
# ---------------------------------------------------------------------------

JS_HELPERS = """
async function readAll(readable) {
    const reader = readable.getReader();
    const chunks = [];
    let totalLength = 0;
    while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        chunks.push(value);
        totalLength += value.byteLength;
    }
    const result = new Uint8Array(totalLength);
    let offset = 0;
    for (const chunk of chunks) {
        result.set(chunk, offset);
        offset += chunk.byteLength;
    }
    return result;
}

async function writeAll(writable, data) {
    const writer = writable.getWriter();
    await writer.write(data);
    await writer.close();
}

async function readAllString(readable) {
    const bytes = await readAll(readable);
    return new TextDecoder().decode(bytes);
}

async function writeAllString(writable, str) {
    await writeAll(writable, new TextEncoder().encode(str));
}
"""


def _webtransport_connect_js(port: int, hash_b64: str) -> str:
    """Return JS code that declares ``url`` and ``transportOptions`` variables."""
    return f"""
        const url = "https://127.0.0.1:{port}";
        const hashBytes = Uint8Array.from(atob("{hash_b64}"), c => c.charCodeAt(0));
        const transportOptions = {{
            serverCertificateHashes: [{{
                algorithm: "sha-256",
                value: hashBytes.buffer,
            }}],
        }};
    """


# ---------------------------------------------------------------------------
# Layer 1: Primitive fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def server_cert() -> tuple[bytes, bytes, str]:
    """Generate a self-signed cert and return ``(cert_der, key_der, cert_hash_b64)``."""
    cert, key = web_transport.generate_self_signed(["localhost", "127.0.0.1", "::1"])
    raw_hash = web_transport.certificate_hash(cert)
    hash_b64 = base64.b64encode(raw_hash).decode("ascii")
    return cert, key, hash_b64


@pytest_asyncio.fixture(loop_scope="session", scope="session")
async def browser() -> AsyncIterator[Browser]:
    """Launch headless Chromium once for the entire test session."""
    async with async_playwright() as pw:
        br = await pw.chromium.launch(headless=True)
        try:
            yield br
        finally:
            await br.close()


@pytest_asyncio.fixture(loop_scope="session")
async def page(browser: Browser) -> AsyncIterator[Page]:
    """Fresh browser page in a secure context (``http://localhost/__test__``).

    ``WebTransport()`` requires a secure context.  ``http://localhost`` is
    treated as secure in Chromium, so we use ``page.route()`` to intercept
    the navigation and serve a minimal HTML page — no real HTTP server needed.
    """
    context = await browser.new_context()

    async def intercept(route: Route) -> None:
        await route.fulfill(
            status=200,
            content_type="text/html",
            body="<!doctype html><title>test</title>",
        )

    try:
        pg = await context.new_page()
        await pg.route("http://localhost/__test__", intercept)
        await pg.goto("http://localhost/__test__")
        yield pg
    finally:
        await context.close()


# ---------------------------------------------------------------------------
# Layer 2: Server factory
# ---------------------------------------------------------------------------


@pytest.fixture
def start_server(
    server_cert: tuple[bytes, bytes, str],
) -> ServerFactory:
    """Return a factory that creates async context managers for WebTransport servers.

    Usage::

        async with start_server() as (server, port, cert_hash_b64):
            ...
        async with start_server(max_idle_timeout=1) as (server, port, cert_hash_b64):
            ...
    """
    cert, key, hash_b64 = server_cert

    @asynccontextmanager
    async def _factory(
        **server_kwargs: str | float | None,
    ) -> AsyncIterator[tuple[web_transport.Server, int, str]]:
        kw = {
            "certificate_chain": [cert],
            "private_key": key,
            "bind": "127.0.0.1:0",
        }
        kw.update(server_kwargs)
        async with web_transport.Server(**kw) as server:  # type: ignore[arg-type]
            _, port = server.local_addr
            yield server, port, hash_b64

    return _factory


# ---------------------------------------------------------------------------
# Layer 3: JS evaluation helper
# ---------------------------------------------------------------------------


@pytest.fixture
def run_js(page: Page) -> RunJS:
    """Connect to a WebTransport server, execute JS, then close.

    Returns an async callable::

        result = await run_js(port, hash_b64, '''
            const stream = await transport.createBidirectionalStream();
            await writeAllString(stream.writable, "hello");
            return await readAllString(stream.readable);
        ''')

    The JS body has access to ``transport`` (a ready ``WebTransport``)
    and the helper functions: ``readAll``, ``writeAll``, ``readAllString``,
    ``writeAllString``.
    """

    async def _run(port: int, hash_b64: str, js_body: str) -> Any:
        setup = _webtransport_connect_js(port, hash_b64)
        script = f"""
            async () => {{
                {JS_HELPERS}
                {setup}
                const transport = new WebTransport(url, transportOptions);
                await transport.ready;
                try {{
                    {js_body}
                }} finally {{
                    transport.close();
                }}
            }}
        """
        return await page.evaluate(script)

    return _run


# ---------------------------------------------------------------------------
# Layer 3b: Raw JS evaluation helper (no auto-connect)
# ---------------------------------------------------------------------------


@pytest.fixture
def run_js_raw(page: Page) -> RunJSRaw:
    """Execute JS without auto-creating a WebTransport connection.

    Use this for tests that need to control the connection lifecycle
    themselves — e.g. observing ``transport.ready`` rejection or
    ``transport.closed`` resolution.

    The JS body has access to the helper functions (``readAll``, ``writeAll``,
    ``readAllString``, ``writeAllString``) but must create and manage the
    ``WebTransport`` instance itself.
    """

    async def _run(js_body: str) -> Any:
        script = f"""async () => {{ {JS_HELPERS}\n{js_body} }}"""
        return await page.evaluate(script)

    return _run
