"""WebTransport bindings for Python.

This module exposes a hand-written, backwards-compatible API on top of
the UniFFI-generated ``web_transport._uniffi`` module. Every method that
can fail wraps the underlying call in a ``try / except`` and re-raises
through :func:`._errors.reraise`, so callers see the legacy exception
hierarchy (:class:`SessionClosedByPeer`, :class:`StreamClosedLocally`,
etc.) rather than the flat ``_uniffi.WebTransportError``.
"""

from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator
from types import TracebackType
from typing import Literal

from web_transport import _uniffi
from web_transport._crypto import certificate_hash, generate_self_signed
from web_transport._errors import (
    ConnectError,
    DatagramError,
    DatagramNotSupportedError,
    DatagramTooLargeError,
    ProtocolError,
    SessionClosed,
    SessionClosedByPeer,
    SessionClosedLocally,
    SessionError,
    SessionRejected,
    SessionTimeout,
    StreamClosed,
    StreamClosedByPeer,
    StreamClosedLocally,
    StreamError,
    StreamIncompleteReadError,
    StreamTooLongError,
    WebTransportError,
    _UniffiError,
    reraise,
)

# Re-export the _errors module under its private name so tests / advanced
# users can reach it if needed; not part of __all__.
from web_transport import _errors  # noqa: F401

__all__ = [
    "generate_self_signed",
    "certificate_hash",
    "WebTransportError",
    "SessionError",
    "ConnectError",
    "SessionRejected",
    "SessionClosed",
    "SessionClosedByPeer",
    "SessionClosedLocally",
    "SessionTimeout",
    "ProtocolError",
    "StreamError",
    "StreamClosed",
    "StreamClosedByPeer",
    "StreamClosedLocally",
    "StreamTooLongError",
    "StreamIncompleteReadError",
    "DatagramError",
    "DatagramTooLargeError",
    "DatagramNotSupportedError",
    "Server",
    "SessionRequest",
    "Client",
    "Session",
    "SendStream",
    "RecvStream",
]


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

_CongestionStr = Literal["default", "throughput", "low_latency"]


def _congestion(value: _CongestionStr) -> _uniffi.CongestionControl:
    """Translate the legacy string into a UniFFI ``CongestionControl`` enum."""
    if value == "default":
        return _uniffi.CongestionControl.DEFAULT
    if value == "throughput":
        return _uniffi.CongestionControl.THROUGHPUT
    if value == "low_latency":
        return _uniffi.CongestionControl.LOW_LATENCY
    raise ValueError(f"unknown congestion_control: {value!r}")


def _addr_tuple(addr: _uniffi.RemoteAddress) -> tuple[str, int]:
    """Convert UniFFI ``RemoteAddress`` record to a legacy ``(host, port)`` tuple."""
    return (addr.host, addr.port)


# ---------------------------------------------------------------------------
# SendStream / RecvStream
# ---------------------------------------------------------------------------


class SendStream:
    """A writable QUIC stream."""

    def __init__(self, inner: _uniffi.SendStream) -> None:
        self._inner = inner

    async def __aenter__(self) -> SendStream:
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: TracebackType | None,
    ) -> None:
        # Mirror the legacy semantics: on exception, reset; on clean exit,
        # finish. Both errors during cleanup are swallowed to avoid masking
        # the original exception (Python will set __context__).
        if exc_type is not None:
            try:
                self._inner.reset(0)
            except _UniffiError:
                pass
            # Yield to the event loop so the underlying FFI runtime gets a
            # chance to transmit RESET_STREAM. The legacy PyO3 binding made
            # __aexit__ truly async (it awaited the inner lock), which gave
            # the asyncio loop a scheduling tick — without an equivalent
            # yield, the peer's accept_bi/accept_uni never sees the stream
            # before we tear it down.
            await asyncio.sleep(0)
            return None
        try:
            await self._inner.finish()
        except _UniffiError as e:
            try:
                reraise(e)
            except StreamClosedLocally:
                # Already finished — fine.
                pass
        return None

    async def write(self, data: bytes) -> None:
        try:
            await self._inner.write(data)
        except _UniffiError as e:
            reraise(e)

    async def write_some(self, data: bytes) -> int:
        try:
            return await self._inner.write_some(data)
        except _UniffiError as e:
            reraise(e)
            raise  # unreachable, satisfies type checker

    async def finish(self) -> None:
        try:
            await self._inner.finish()
        except _UniffiError as e:
            reraise(e)

    def reset(self, error_code: int = 0) -> None:
        try:
            self._inner.reset(error_code)
        except _UniffiError as e:
            reraise(e)

    async def wait_closed(self) -> int | None:
        try:
            return await self._inner.wait_closed()
        except _UniffiError as e:
            reraise(e)
            raise  # unreachable

    @property
    def priority(self) -> int:
        return self._inner.priority()

    @priority.setter
    def priority(self, value: int) -> None:
        self._inner.set_priority(value)


class RecvStream:
    """A readable QUIC stream."""

    def __init__(self, inner: _uniffi.RecvStream) -> None:
        self._inner = inner
        self._eof = False

    async def __aenter__(self) -> RecvStream:
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: TracebackType | None,
    ) -> None:
        # Always best-effort STOP_SENDING on exit; swallow errors so they
        # don't mask anything in flight. Yield after to give the FFI runtime
        # a chance to transmit the frame (see SendStream.__aexit__).
        try:
            self._inner.stop(0)
        except _UniffiError:
            pass
        await asyncio.sleep(0)
        return None

    def __aiter__(self) -> AsyncIterator[bytes]:
        return self

    async def __anext__(self) -> bytes:
        chunk = await self.read(65536)
        if not chunk:
            raise StopAsyncIteration
        return chunk

    async def read(self, n: int = -1, *, limit: int | None = None) -> bytes:
        """Read up to *n* bytes, or until EOF if *n* == -1."""
        try:
            if n < 0:
                cap = limit if limit is not None else 2**63 - 1
                data = await self._inner.read_to_end(cap)
                self._eof = True
                return data
            if self._eof:
                return b""
            data = await self._inner.read(n)
            if not data:
                self._eof = True
            return data
        except _UniffiError as e:
            reraise(e)
            raise  # unreachable

    async def readexactly(self, n: int) -> bytes:
        try:
            return await self._inner.read_exact(n)
        except _UniffiError as e:
            reraise(e)
            raise  # unreachable

    def stop(self, error_code: int = 0) -> None:
        try:
            self._inner.stop(error_code)
        except _UniffiError as e:
            reraise(e)

    async def wait_closed(self) -> int | None:
        try:
            return await self._inner.wait_closed()
        except _UniffiError as e:
            reraise(e)
            raise  # unreachable


# ---------------------------------------------------------------------------
# Session
# ---------------------------------------------------------------------------


class Session:
    """An established WebTransport session."""

    def __init__(self, inner: _uniffi.Session) -> None:
        self._inner = inner
        # quinn's max_datagram_size can grow during the session as path-MTU
        # discovery learns a larger MTU. Cache the initial value so the
        # public property is stable and the boundary check in
        # send_datagram is predictable for callers (legacy behavior).
        self._max_datagram_size = inner.max_datagram_size()

    async def __aenter__(self) -> Session:
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: TracebackType | None,
    ) -> None:
        try:
            self._inner.close(0, "")
        except _UniffiError:
            pass
        # Mirror the legacy PyO3 binding: await closed() so the close
        # capsule actually transmits before __aexit__ returns. Without
        # this, peer-side observers won't see the close in time.
        try:
            await self._inner.wait_closed()
        except _UniffiError:
            pass
        return None

    # -- Streams --------------------------------------------------------

    async def open_bi(self) -> tuple[SendStream, RecvStream]:
        try:
            bi = await self._inner.open_bi()
        except _UniffiError as e:
            reraise(e)
            raise  # unreachable
        return SendStream(bi.send), RecvStream(bi.recv)

    async def open_uni(self) -> SendStream:
        try:
            send = await self._inner.open_uni()
        except _UniffiError as e:
            reraise(e)
            raise  # unreachable
        return SendStream(send)

    async def accept_bi(self) -> tuple[SendStream, RecvStream]:
        try:
            bi = await self._inner.accept_bi()
        except _UniffiError as e:
            reraise(e)
            raise  # unreachable
        return SendStream(bi.send), RecvStream(bi.recv)

    async def accept_uni(self) -> RecvStream:
        try:
            recv = await self._inner.accept_uni()
        except _UniffiError as e:
            reraise(e)
            raise  # unreachable
        return RecvStream(recv)

    # -- Datagrams ------------------------------------------------------

    def send_datagram(self, data: bytes) -> None:
        # Enforce the strict cached boundary so callers see the same limit
        # at any point in the session (quinn's live max grows with path-MTU
        # discovery; without this check, send_datagram(max+1) might or
        # might not succeed depending on what was sent earlier).
        if len(data) > self._max_datagram_size:
            raise DatagramTooLargeError(
                f"datagram size {len(data)} exceeds maximum {self._max_datagram_size}"
            )
        try:
            self._inner.send_datagram(data)
        except _UniffiError as e:
            reraise(e)

    async def receive_datagram(self) -> bytes:
        try:
            return await self._inner.receive_datagram()
        except _UniffiError as e:
            reraise(e)
            raise  # unreachable

    # -- Lifecycle ------------------------------------------------------

    def close(self, code: int = 0, reason: str = "") -> None:
        # Legacy contract: `close(code)` with an out-of-range u32 raises
        # OverflowError (uniffi would raise ValueError for the same case).
        if not 0 <= code < 2**32:
            raise OverflowError(f"code out of range for u32: {code}")
        try:
            self._inner.close(code, reason)
        except _UniffiError:
            pass

    async def wait_closed(self) -> None:
        # Legacy contract: never raises; absorbs the close reason.
        try:
            await self._inner.wait_closed()
        except _UniffiError:
            pass

    # -- Properties -----------------------------------------------------

    @property
    def close_reason(self) -> SessionError | None:
        """The structured close reason, or ``None`` if the session is still open.

        Returns the same subclass that the originating operation would raise —
        :class:`SessionClosedByPeer` for peer-initiated closes (with
        ``.source``, ``.code``, ``.reason`` attributes), :class:`SessionTimeout`
        for idle timeout, :class:`SessionClosedLocally` for local close, etc.
        Always a :class:`SessionError` subclass because the FFI only emits
        session-level variants on a closed session.
        """
        raw = self._inner.close_reason()
        if raw is None:
            return None
        # Translate the structured FFI variant into the legacy subclass.
        try:
            reraise(raw)  # type: ignore[arg-type]
        except SessionError as exc:
            return exc
        except WebTransportError as exc:
            # Defensive: a non-session WebTransportError variant somehow
            # surfaced. Wrap it so the return type stays SessionError.
            return SessionError(str(exc))
        # `reraise()` always raises, but ty doesn't know that.
        return SessionError(str(raw))

    @property
    def max_datagram_size(self) -> int:
        # Stable across the session; see note in __init__.
        return self._max_datagram_size

    @property
    def remote_address(self) -> tuple[str, int]:
        return _addr_tuple(self._inner.remote_address())

    @property
    def rtt(self) -> float:
        return self._inner.rtt()


# ---------------------------------------------------------------------------
# SessionRequest
# ---------------------------------------------------------------------------


class SessionRequest:
    """An incoming WebTransport session request."""

    def __init__(self, inner: _uniffi.SessionRequest) -> None:
        self._inner = inner

    @property
    def url(self) -> str:
        return self._inner.url()

    @property
    def remote_address(self) -> tuple[str, int]:
        return _addr_tuple(self._inner.remote_address())

    async def accept(self) -> Session:
        try:
            sess = await self._inner.accept()
        except _UniffiError as e:
            reraise(e)
            raise  # unreachable
        return Session(sess)

    async def reject(self, status_code: int = 404) -> None:
        try:
            await self._inner.reject(status_code)
        except _UniffiError as e:
            reraise(e)


# ---------------------------------------------------------------------------
# Client
# ---------------------------------------------------------------------------


class Client:
    """WebTransport client."""

    def __init__(
        self,
        *,
        server_certificate_hashes: list[bytes] | None = None,
        no_cert_verification: bool = False,
        congestion_control: _CongestionStr = "default",
        max_idle_timeout: float | None = 30,
        keep_alive_interval: float | None = None,
    ) -> None:
        cfg = _uniffi.ClientConfig(
            server_certificate_hashes=server_certificate_hashes,
            no_cert_verification=no_cert_verification,
            congestion_control=_congestion(congestion_control),
            max_idle_timeout_secs=max_idle_timeout,
            keep_alive_interval_secs=keep_alive_interval,
        )
        try:
            self._inner = _uniffi.Client(cfg)
        except _UniffiError as e:
            reraise(e)

    async def __aenter__(self) -> Client:
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: TracebackType | None,
    ) -> None:
        try:
            self._inner.close(0, "")
        except _UniffiError:
            pass
        try:
            await self._inner.wait_closed()
        except _UniffiError:
            pass
        return None

    def close(self, code: int = 0, reason: str = "") -> None:
        try:
            self._inner.close(code, reason)
        except _UniffiError as e:
            reraise(e)

    async def wait_closed(self) -> None:
        try:
            await self._inner.wait_closed()
        except _UniffiError:
            # Legacy semantics: wait_closed is non-raising.
            pass

    async def connect(self, url: str) -> Session:
        try:
            sess = await self._inner.connect(url)
        except _UniffiError as e:
            reraise(e)
            raise  # unreachable
        return Session(sess)


# ---------------------------------------------------------------------------
# Server
# ---------------------------------------------------------------------------


class Server:
    """WebTransport server."""

    def __init__(
        self,
        *,
        certificate_chain: list[bytes],
        private_key: bytes,
        bind: str = "[::]:4433",
        congestion_control: _CongestionStr = "default",
        max_idle_timeout: float | None = 30,
        keep_alive_interval: float | None = None,
    ) -> None:
        cfg = _uniffi.ServerConfig(
            certificate_chain=certificate_chain,
            private_key=private_key,
            bind=bind,
            congestion_control=_congestion(congestion_control),
            max_idle_timeout_secs=max_idle_timeout,
            keep_alive_interval_secs=keep_alive_interval,
        )
        try:
            self._inner = _uniffi.Server(cfg)
        except _UniffiError as e:
            reraise(e)

    async def __aenter__(self) -> Server:
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: TracebackType | None,
    ) -> None:
        try:
            self._inner.close(0, "")
        except _UniffiError:
            pass
        try:
            await self._inner.wait_closed()
        except (_UniffiError, AttributeError):
            pass
        return None

    def __aiter__(self) -> AsyncIterator[SessionRequest]:
        return self

    async def __anext__(self) -> SessionRequest:
        req = await self.accept()
        if req is None:
            raise StopAsyncIteration
        return req

    async def accept(self) -> SessionRequest | None:
        try:
            req = await self._inner.accept()
        except _UniffiError as e:
            reraise(e)
            raise  # unreachable
        if req is None:
            return None
        return SessionRequest(req)

    def close(self, code: int = 0, reason: str = "") -> None:
        try:
            self._inner.close(code, reason)
        except _UniffiError as e:
            reraise(e)

    async def wait_closed(self) -> None:
        # The FFI Server has no `wait_closed`; provide a no-op shim so
        # the legacy API still resolves.
        fn = getattr(self._inner, "wait_closed", None)
        if fn is None:
            return None
        try:
            await fn()
        except _UniffiError:
            pass

    @property
    def local_addr(self) -> tuple[str, int]:
        return _addr_tuple(self._inner.local_addr())

    def reload_certificates(
        self,
        certificate_chain: list[bytes],
        private_key: bytes,
    ) -> None:
        try:
            self._inner.reload_certificates(certificate_chain, private_key)
        except _UniffiError as e:
            reraise(e)
