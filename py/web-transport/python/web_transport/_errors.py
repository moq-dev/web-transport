"""Legacy exception hierarchy, reconstructed on top of the UniFFI enum.

UniFFI emits a single ``WebTransportError`` exception class with one
variant subclass per error case (``WebTransportError.Connect``,
``WebTransportError.SessionClosedByPeer``, etc.). User code from the
PyO3 era catches more specific subclasses like :class:`SessionClosedByPeer`
or :class:`StreamClosedLocally`, and inspects attributes such as
``.source``, ``.code``, ``.reason``, ``.status_code``, ``.kind``, ``.partial``,
``.expected``, and ``.limit``.

:func:`reraise` inspects which UniFFI variant was raised and re-raises
the matching legacy subclass, copying structured fields directly from
the variant instance.
"""

from __future__ import annotations

from typing import Literal

from web_transport import _uniffi

# UniFFI generates two classes named WebTransportError in the same module:
# the real Exception subclass (renamed to a private helper), and a container
# class that holds the variant subclasses. ty/pyright resolve the public name
# to the container, which isn't an Exception subclass. Reach through one of
# the variants to recover the real base class — every variant inherits from
# the original Exception subclass — so `except _UniffiError` is a single
# valid catch type that covers every variant.
_UniffiError = _uniffi.WebTransportError.Connect.__bases__[0]
assert issubclass(_UniffiError, Exception)

# ---------------------------------------------------------------------------
# Class hierarchy (matches the original PyO3 binding)
# ---------------------------------------------------------------------------


class WebTransportError(Exception):
    """Base exception for all web-transport errors."""


# -- Session ----------------------------------------------------------------


class SessionError(WebTransportError):
    """Base class for session-level errors."""


class ConnectError(SessionError):
    """Failed to establish a WebTransport session."""


class SessionRejected(ConnectError):
    """The server rejected the WebTransport session request."""

    status_code: int


class SessionClosed(SessionError):
    """The session was closed (by either side)."""


class SessionClosedByPeer(SessionClosed):
    """The peer closed the session."""

    source: Literal["session", "application", "transport", "connection-reset"]
    code: int | None
    reason: str


class SessionClosedLocally(SessionClosed):
    """The local application already closed this session."""


class SessionTimeout(SessionError):
    """The session timed out due to inactivity."""


class ProtocolError(SessionError):
    """A QUIC or HTTP/3 protocol violation occurred."""


# -- Stream -----------------------------------------------------------------


class StreamError(WebTransportError):
    """Base class for stream-level errors."""


class StreamClosed(StreamError):
    """The stream was closed (by either side)."""


class StreamClosedByPeer(StreamClosed):
    """The peer closed this stream via STOP_SENDING or RESET_STREAM."""

    kind: Literal["reset", "stop"]
    code: int


class StreamClosedLocally(StreamClosed):
    """The stream was already finished or reset locally."""


class StreamTooLongError(StreamError):
    """A read exceeded the maximum allowed data size."""

    limit: int


class StreamIncompleteReadError(StreamError):
    """EOF was reached before enough bytes were read."""

    expected: int
    partial: bytes


# -- Datagram ---------------------------------------------------------------


class DatagramError(WebTransportError):
    """Base class for datagram-level errors."""


class DatagramTooLargeError(DatagramError):
    """The datagram payload exceeds the maximum size for this session."""


class DatagramNotSupportedError(DatagramError):
    """Datagrams are not supported by the peer or are disabled locally."""

    reason: str


# ---------------------------------------------------------------------------
# Translation
# ---------------------------------------------------------------------------


def reraise(err: BaseException) -> None:
    """Translate a ``_uniffi.WebTransportError`` into the legacy subclass.

    Call as ``_errors.reraise(e)`` from an ``except`` clause — this function
    always raises and never returns. The original exception is preserved
    via ``raise ... from err``.
    """
    # Anything that isn't from uniffi is passed through unchanged so we
    # don't accidentally swallow KeyboardInterrupt / asyncio.CancelledError.
    if not isinstance(err, _uniffi.WebTransportError):
        raise err

    # --- Session ----------------------------------------------------------

    if isinstance(err, _uniffi.WebTransportError.Connect):
        # Tuple variant: message is in err[0].
        raise ConnectError(err[0]) from err

    if isinstance(err, _uniffi.WebTransportError.SessionRejected):
        new = SessionRejected(err.detail)
        new.status_code = err.status_code
        raise new from err

    if isinstance(err, _uniffi.WebTransportError.SessionClosedByPeer):
        closed_by = err.closed_by
        code = err.code
        reason = err.reason
        if closed_by == "session":
            msg = f"peer closed session with code {code}: {reason}"
        elif closed_by == "application":
            msg = f"peer application closed with code {code}: {reason}"
        elif closed_by == "transport":
            msg = f"transport closed with code {code}: {reason}"
        elif closed_by == "connection-reset":
            msg = "peer sent stateless reset"
        else:
            msg = f"session closed by peer ({closed_by}): {reason}"
        new = SessionClosedByPeer(msg)
        # Legacy attribute name was `source`, not `closed_by`.
        new.source = closed_by
        new.code = code
        new.reason = reason
        raise new from err

    if isinstance(err, _uniffi.WebTransportError.SessionClosedLocally):
        raise SessionClosedLocally("session closed locally") from err

    if isinstance(err, _uniffi.WebTransportError.SessionTimeout):
        raise SessionTimeout("session timed out") from err

    if isinstance(err, _uniffi.WebTransportError.Protocol):
        raise ProtocolError(err[0]) from err

    # --- Stream -----------------------------------------------------------

    if isinstance(err, _uniffi.WebTransportError.StreamClosedByPeer):
        new = StreamClosedByPeer(f"stream {err.kind} by peer with code {err.code}")
        new.kind = err.kind
        new.code = err.code
        raise new from err

    if isinstance(err, _uniffi.WebTransportError.StreamClosedLocally):
        raise StreamClosedLocally("stream closed locally") from err

    if isinstance(err, _uniffi.WebTransportError.StreamTooLong):
        new = StreamTooLongError(f"stream data exceeded {err.limit} byte limit")
        new.limit = err.limit
        raise new from err

    if isinstance(err, _uniffi.WebTransportError.StreamIncompleteRead):
        new = StreamIncompleteReadError(
            f"expected {err.expected} bytes, got {err.got} before EOF"
        )
        new.expected = err.expected
        new.partial = err.partial
        raise new from err

    # --- Datagram ---------------------------------------------------------

    if isinstance(err, _uniffi.WebTransportError.DatagramTooLarge):
        raise DatagramTooLargeError("datagram too large") from err

    if isinstance(err, _uniffi.WebTransportError.DatagramNotSupported):
        new = DatagramNotSupportedError(f"datagrams not supported: {err.reason}")
        new.reason = err.reason
        raise new from err

    # --- Miscellaneous ----------------------------------------------------

    if isinstance(err, _uniffi.WebTransportError.InvalidArgument):
        # Mirrors the PyO3 binding which raised plain ValueError for
        # invalid arguments rather than WebTransportError.
        raise ValueError(err[0]) from err

    if isinstance(err, _uniffi.WebTransportError.Io):
        raise WebTransportError(err[0]) from err

    if isinstance(err, _uniffi.WebTransportError.Cancelled):
        # In the legacy binding, a cancelled stream operation surfaced as
        # StreamClosedLocally. Sessions don't get Cancelled, so this is
        # a safe default.
        raise StreamClosedLocally("cancelled") from err

    # Catch-all
    raise WebTransportError(str(err)) from err
