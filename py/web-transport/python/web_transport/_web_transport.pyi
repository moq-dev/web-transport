"""Type stubs for the web-transport native module."""

from collections.abc import AsyncIterator
from typing import Literal
from types import TracebackType

# ---------------------------------------------------------------------------
# Free functions
# ---------------------------------------------------------------------------

def generate_self_signed(
    subject_alt_names: list[str],
) -> tuple[bytes, bytes]:
    """Generate a self-signed certificate and private key.

    Uses ECDSA P-256 and a 14-day validity period (the maximum allowed by
    WebTransport for certificate pinning).

    Args:
        subject_alt_names: SANs for the certificate
            (e.g. ``["localhost", "127.0.0.1", "::1"]``).
            Each entry is parsed as an IP address if possible, otherwise
            treated as a DNS name.

    Returns:
        A ``(certificate_der, private_key_der)`` tuple of raw DER bytes.

    Raises:
        ValueError: If *subject_alt_names* is empty or contains values that
            are neither valid DNS names nor IP addresses.
    """
    ...

def certificate_hash(certificate_der: bytes) -> bytes:
    """Compute the SHA-256 fingerprint of a DER-encoded certificate.

    Args:
        certificate_der: Raw DER-encoded certificate bytes.

    Returns:
        32 bytes (SHA-256 digest).
    """
    ...

# ---------------------------------------------------------------------------
# Exceptions
#
# WebTransportError                     Base for all web-transport errors
# ├── SessionError                      Session-level failures
# │   ├── ConnectError                  Failed to establish a session
# │   │   └── SessionRejected           Server rejected with HTTP status (.status_code)
# │   ├── SessionClosed                 Session closed (either side)
# │   │   ├── SessionClosedByPeer       Peer closed (.source, .code?, .reason?)
# │   │   └── SessionClosedLocally      Local side already closed the session
# │   ├── SessionTimeout                Idle timeout expired
# │   └── ProtocolError                 QUIC / HTTP/3 protocol violation
# ├── StreamError                       Stream-level failures
# │   ├── StreamClosed                  Stream closed (either side)
# │   │   ├── StreamClosedByPeer        Peer stopped/reset (.kind, .code)
# │   │   └── StreamClosedLocally       Stream already finished/reset locally
# │   ├── StreamTooLongError            read(-1) data exceeded size limit
# │   └── StreamIncompleteReadError     EOF before expected bytes (.expected, .partial)
# └── DatagramError                     Datagram-level failures
#     ├── DatagramTooLargeError         Payload exceeds max datagram size
#     └── DatagramNotSupportedError     Datagrams unavailable (.reason)
# ---------------------------------------------------------------------------

class WebTransportError(Exception):
    """Base exception for all web-transport errors."""

    ...

# -- Session errors ---------------------------------------------------------

class SessionError(WebTransportError):
    """Base class for session-level errors."""

    ...

class ConnectError(SessionError):
    """Failed to establish a WebTransport session."""

    ...

class SessionRejected(ConnectError):
    """The server rejected the WebTransport session request.

    Raised when the server responds to the HTTP/3 extended CONNECT
    request with a non-200 status code (e.g. 403, 404).

    Attributes:
        status_code: The HTTP status code returned by the server.
    """

    status_code: int
    ...

class SessionClosed(SessionError):
    """The session was closed (by either side).

    Catch this to handle both :class:`SessionClosedByPeer` and
    :class:`SessionClosedLocally` uniformly.
    """

    ...

class SessionClosedByPeer(SessionClosed):
    """The peer closed the session.

    Attributes:
        source: How the session was closed:
            - ``"session"`` — the peer sent a WebTransport
              ``CLOSE_WEBTRANSPORT_SESSION`` capsule; *code* and *reason*
              are meaningful.
            - ``"application"`` — the peer's QUIC stack sent
              ``CONNECTION_CLOSE`` with an application error code (HTTP/3
              layer); *code* may be meaningful, *reason* may be present.
            - ``"transport"`` — the peer's QUIC stack closed the connection
              (e.g. protocol violation detected by the peer).
            - ``"connection-reset"`` — the peer sent a stateless reset,
              typically because it lost all connection state (e.g. after a
              restart).
        code: Error code (when ``source`` is ``"session"`` or
            ``"application"``), or ``None`` for transport-level closes and
            connection resets.
        reason: Human-readable close reason, or ``""`` if not provided.
    """

    source: Literal["session", "application", "transport", "connection-reset"]
    code: int | None
    reason: str

class SessionClosedLocally(SessionClosed):
    """The local application already closed this session.

    Raised when an operation is attempted on a session that was closed by a
    prior call to :meth:`Session.close`.
    """

    ...

class SessionTimeout(SessionError):
    """The session timed out due to inactivity."""

    ...

class ProtocolError(SessionError):
    """A QUIC or HTTP/3 protocol violation occurred.

    Also raised when the peer sends a stream STOP_SENDING or RESET_STREAM
    with an HTTP/3 error code that cannot be mapped to a valid WebTransport
    error code.
    """

    ...

# -- Stream errors ----------------------------------------------------------

class StreamError(WebTransportError):
    """Base class for stream-level errors."""

    ...

class StreamClosed(StreamError):
    """The stream was closed (by either side).

    Catch this to handle both :class:`StreamClosedByPeer` and
    :class:`StreamClosedLocally` uniformly.
    """

    ...

class StreamClosedByPeer(StreamClosed):
    """The peer closed this stream via STOP_SENDING or RESET_STREAM.

    Attributes:
        kind: How the stream was closed:
            - ``"reset"`` — the peer sent RESET_STREAM (receive side),
              abandoning transmission of data on the stream.
            - ``"stop"`` — the peer sent STOP_SENDING (send side),
              requesting that we stop writing to the stream.
        code: The application error code from the peer.
    """

    kind: Literal["reset", "stop"]
    code: int

class StreamClosedLocally(StreamClosed):
    """The stream was already finished or reset locally."""

    ...

class StreamTooLongError(StreamError):
    """A ``read()`` call exceeded the maximum allowed data size.

    Raised when reading until EOF (``read(-1, limit=...)``) and the incoming
    data exceeds *limit* before the stream finishes.

    Attributes:
        limit: The byte limit that was exceeded.
    """

    limit: int

class StreamIncompleteReadError(StreamError):
    """EOF was reached before enough bytes were read.

    Raised by :meth:`RecvStream.readexactly` when the stream finishes
    before *n* bytes have been received.  Modeled after
    :class:`asyncio.IncompleteReadError`.

    Attributes:
        expected: Number of bytes that were requested.
        partial: The bytes that were successfully read before EOF.
    """

    expected: int
    partial: bytes

# -- Datagram errors --------------------------------------------------------

class DatagramError(WebTransportError):
    """Base class for datagram-level errors."""

    ...

class DatagramTooLargeError(DatagramError):
    """The datagram payload exceeds the maximum size for this session."""

    ...

class DatagramNotSupportedError(DatagramError):
    """Datagrams are not supported by the peer or are disabled locally.

    Attributes:
        reason: Why datagrams are unavailable:
            - ``"unsupported_by_peer"`` — the peer does not support
              receiving datagram frames.
            - ``"disabled_locally"`` — datagram support is disabled locally.
    """

    reason: Literal["unsupported_by_peer", "disabled_locally"]

    ...

# ---------------------------------------------------------------------------
# Core classes
# ---------------------------------------------------------------------------

class Server:
    """WebTransport server.

    Listens for incoming sessions over QUIC. Use as an async context manager
    and iterate with ``async for`` to accept requests::

        async with Server(certificate_chain=[cert], private_key=key) as srv:
            async for request in srv:
                session = await request.accept()

    Args:
        certificate_chain: DER-encoded certificates (leaf first).
        private_key: DER-encoded PKCS#8 private key.
        bind: Address to listen on (default ``"[::]:4433"``).
        congestion_control: Algorithm to use — ``"default"``, ``"throughput"``,
            or ``"low_latency"`` (default ``"default"``).
        max_idle_timeout: Maximum idle time in seconds before the connection
            is closed. ``None`` disables the timeout (default ``30``).
        keep_alive_interval: Interval in seconds between QUIC keep-alive
            pings. ``None`` disables keep-alives (default ``None``).
    """

    def __init__(
        self,
        *,
        certificate_chain: list[bytes],
        private_key: bytes,
        bind: str = "[::]:4433",
        congestion_control: Literal["default", "throughput", "low_latency"] = "default",
        max_idle_timeout: float | None = 30,
        keep_alive_interval: float | None = None,
    ) -> None:
        """Create a new WebTransport server.

        Raises:
            ValueError: If *bind* is not a valid socket address, *private_key*
                is not a valid PKCS#8 DER-encoded key, the certificate chain
                is invalid, *congestion_control* is not a recognized algorithm,
                or *max_idle_timeout* is out of range.
        """
        ...

    async def __aenter__(self) -> Server: ...
    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: TracebackType | None,
    ) -> None: ...
    def __aiter__(self) -> AsyncIterator[SessionRequest]: ...
    async def __anext__(self) -> SessionRequest: ...
    async def accept(self) -> SessionRequest | None:
        """Wait for the next incoming session request.

        Returns ``None`` when the server has been closed.
        """
        ...

    def close(self, code: int = 0, reason: str = "") -> None:
        """Close all connections and stop accepting new ones.

        This is a QUIC endpoint-level operation: it sends a
        ``CONNECTION_CLOSE`` frame to every connected client.  The *code*
        is placed on the wire as-is (no WebTransport-to-HTTP/3 mapping).
        Use :meth:`Session.close` to close an individual session with a
        WebTransport application error code.

        This method is synchronous — it queues the close frames and
        returns immediately.  Call ``await wait_closed()`` afterwards to
        ensure the frames are transmitted to peers.

        Args:
            code: QUIC application error code (default ``0``).
                Must be less than ``2**62``.
            reason: Human-readable close reason (default ``""``).

        Raises:
            ValueError: If *code* is not below ``2**62``.
        """
        ...

    async def wait_closed(self) -> None:
        """Wait for all connections to be cleanly shut down."""
        ...

    @property
    def local_addr(self) -> tuple[str, int]:
        """The local ``(host, port)`` the server is bound to."""
        ...

class SessionRequest:
    """An incoming WebTransport session request.

    Obtained from :meth:`Server.accept` or by iterating a :class:`Server`.
    Inspect the request and call :meth:`accept` or :meth:`reject`.
    """

    @property
    def url(self) -> str:
        """The URL requested by the client."""
        ...

    async def accept(self) -> Session:
        """Accept the session request.

        Returns:
            An established :class:`Session`.

        Raises:
            SessionError: If the request was already accepted or rejected, or
                the underlying connection was lost.
        """
        ...

    async def reject(self, status_code: int = 404) -> None:
        """Reject the session request with an HTTP status code.

        Args:
            status_code: HTTP status code (e.g. ``403``, ``404``, ``429``).

        Raises:
            ValueError: If *status_code* is not a valid HTTP status code.
            SessionError: If the request was already accepted or rejected, or
                the underlying connection was lost.
        """
        ...

class Client:
    """WebTransport client.

    Use as an async context manager::

        async with Client() as client:
            session = await client.connect("https://example.com:4433")

    Certificate verification defaults to the system root CA store. Use
    ``server_certificate_hashes`` to pin specific certificates, or
    ``no_cert_verification=True`` to disable verification entirely (testing
    only).

    Args:
        server_certificate_hashes: Pin against these SHA-256 certificate
            hashes instead of using system roots.
        no_cert_verification: Disable all certificate verification.
            **Dangerous** — only for local development.
        congestion_control: Algorithm to use — ``"default"``, ``"throughput"``,
            or ``"low_latency"`` (default ``"default"``).
        max_idle_timeout: Maximum idle time in seconds before the connection
            is closed. ``None`` disables the timeout (default ``30``).
        keep_alive_interval: Interval in seconds between QUIC keep-alive
            pings. ``None`` disables keep-alives (default ``None``).
    """

    def __init__(
        self,
        *,
        server_certificate_hashes: list[bytes] | None = None,
        no_cert_verification: bool = False,
        congestion_control: Literal["default", "throughput", "low_latency"] = "default",
        max_idle_timeout: float | None = 30,
        keep_alive_interval: float | None = None,
    ) -> None:
        """Create a new WebTransport client.

        Raises:
            ValueError: If *congestion_control* is not a recognized algorithm,
                *max_idle_timeout* is out of range, or the QUIC endpoint
                could not be created.
        """
        ...

    async def __aenter__(self) -> Client: ...
    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: TracebackType | None,
    ) -> None: ...
    def close(self, code: int = 0, reason: str = "") -> None:
        """Close all connections.

        This is a QUIC endpoint-level operation: it sends a
        ``CONNECTION_CLOSE`` frame to every connection on the endpoint.
        The *code* is placed on the wire as-is (no WebTransport-to-HTTP/3
        mapping).  Use :meth:`Session.close` to close an individual
        session with a WebTransport application error code.

        This method is synchronous — it queues the close frames and
        returns immediately.  Call ``await wait_closed()`` afterwards to
        ensure the frames are transmitted to peers.

        Args:
            code: QUIC application error code (default ``0``).
                Must be less than ``2**62``.
            reason: Human-readable close reason (default ``""``).

        Raises:
            ValueError: If *code* is not below ``2**62``.
        """
        ...

    async def wait_closed(self) -> None:
        """Wait for all connections to be cleanly shut down."""
        ...

    async def connect(self, url: str) -> Session:
        """Open a WebTransport session to the given URL.

        Args:
            url: An ``https://`` URL to connect to.

        Returns:
            An established :class:`Session`.

        Raises:
            ValueError: If *url* is not a valid URL.
            ConnectError: If the connection cannot be established (e.g.
                DNS resolution failure, HTTP/3 error, or invalid server name).
            SessionTimeout: If the connection attempt timed out.
            ProtocolError: If a QUIC or HTTP/3 protocol violation occurred
                during the handshake.
        """
        ...

class Session:
    """An established WebTransport session.

    Obtained from :meth:`SessionRequest.accept` (server) or
    :meth:`Client.connect` (client). Use as an async context manager to
    ensure the session is closed on exit::

        async with session:
            send, recv = await session.open_bi()
            ...
    """

    async def __aenter__(self) -> Session: ...
    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: TracebackType | None,
    ) -> None: ...

    # -- Streams ------------------------------------------------------------

    async def open_bi(self) -> tuple[SendStream, RecvStream]:
        """Open a new bidirectional stream.

        Stream creation is a local operation — this method does not wait
        for the peer to call :meth:`accept_bi`.  The returned streams are
        immediately usable: :meth:`~SendStream.write`,
        :meth:`~SendStream.finish`, etc. all succeed regardless of whether
        the peer has accepted the stream yet.

        Returns:
            A ``(send, recv)`` stream pair.

        Raises:
            SessionClosedByPeer: If the peer closed the session.
            SessionClosedLocally: If the session was already closed locally.
            SessionTimeout: If the session timed out.
            ProtocolError: If a QUIC or HTTP/3 protocol violation occurred.
        """
        ...

    async def open_uni(self) -> SendStream:
        """Open a new unidirectional (send-only) stream.

        Stream creation is a local operation — this method does not wait
        for the peer to call :meth:`accept_uni`.  The returned stream is
        immediately usable: :meth:`~SendStream.write`,
        :meth:`~SendStream.finish`, etc. all succeed regardless of whether
        the peer has accepted the stream yet.

        Returns:
            A :class:`SendStream`.

        Raises:
            SessionClosedByPeer: If the peer closed the session.
            SessionClosedLocally: If the session was already closed locally.
            SessionTimeout: If the session timed out.
            ProtocolError: If a QUIC or HTTP/3 protocol violation occurred.
        """
        ...

    async def accept_bi(self) -> tuple[SendStream, RecvStream]:
        """Wait for the peer to open a bidirectional stream.

        .. note::

            Only one ``accept_bi`` call should be awaited at a time per
            session.  Concurrent calls will cause all but one to hang
            indefinitely due to an upstream limitation in the stream
            accept queue.  Use a single accept loop instead::

                while True:
                    send, recv = await session.accept_bi()
                    asyncio.create_task(handle_stream(send, recv))

        Returns:
            A ``(send, recv)`` stream pair.

        Raises:
            SessionClosedByPeer: If the peer closed the session.
            SessionClosedLocally: If the session was already closed locally.
            SessionTimeout: If the session timed out.
            ProtocolError: If a QUIC or HTTP/3 protocol violation occurred.
        """
        ...

    async def accept_uni(self) -> RecvStream:
        """Wait for the peer to open a unidirectional stream.

        .. note::

            Only one ``accept_uni`` call should be awaited at a time per
            session.  See :meth:`accept_bi` for details.

        Returns:
            A :class:`RecvStream`.

        Raises:
            SessionClosedByPeer: If the peer closed the session.
            SessionClosedLocally: If the session was already closed locally.
            SessionTimeout: If the session timed out.
            ProtocolError: If a QUIC or HTTP/3 protocol violation occurred.
        """
        ...

    # -- Datagrams ----------------------------------------------------------

    def send_datagram(self, data: bytes) -> None:
        """Send an unreliable datagram.

        Args:
            data: Payload bytes. Must not exceed :attr:`max_datagram_size`.

        Raises:
            DatagramTooLargeError: If *data* exceeds the maximum datagram size
                for this session.
            DatagramNotSupportedError: If datagrams are not supported by the
                peer or are disabled locally.
            SessionClosedByPeer: If the peer closed the session.
            SessionClosedLocally: If the session was already closed locally.
            SessionTimeout: If the session timed out.
        """
        ...

    async def receive_datagram(self) -> bytes:
        """Wait for and return the next incoming datagram.

        Returns:
            The datagram payload as raw bytes.

        Raises:
            SessionClosedByPeer: If the peer closed the session.
            SessionClosedLocally: If the session was already closed locally.
            SessionTimeout: If the session timed out.
            ProtocolError: If a QUIC or HTTP/3 protocol violation occurred.
        """
        ...

    # -- Lifecycle ----------------------------------------------------------

    def close(self, code: int = 0, reason: str = "") -> None:
        """Close the session.

        Sends a WebTransport ``CLOSE_WEBTRANSPORT_SESSION`` capsule with
        the given *code* and *reason*.  The peer will observe this as a
        :class:`SessionClosedByPeer` with ``source="session"``.

        This method is synchronous — it queues the close capsule and
        returns immediately.  Call ``await wait_closed()`` afterwards to
        ensure the capsule is fully transmitted to the peer before the
        connection is torn down.  Without ``wait_closed()``, the capsule
        may only be partially sent if the connection closes shortly after
        (e.g. the enclosing task or context manager exits), causing the
        peer to see an unexpected error instead of a clean
        :class:`SessionClosedByPeer`.

        This method is idempotent — calling it on an already-closed session
        is a no-op.

        Args:
            code: WebTransport application error code (default ``0``).
                Valid range is ``0`` to ``2**32 - 1``.
            reason: Human-readable close reason (default ``""``).
        """
        ...

    async def wait_closed(self) -> None:
        """Wait until the session is closed (for any reason).

        Returns immediately if the session is already closed. This method
        never raises — it silently absorbs the close reason.  Use
        :attr:`close_reason` afterwards to inspect why the session closed.
        """
        ...

    # -- Properties ---------------------------------------------------------

    @property
    def close_reason(self) -> SessionError | None:
        """The reason the session was closed, or ``None`` if still open.

        Returns the exception that would have been raised by an in-flight
        operation (e.g. :class:`SessionClosedByPeer`, :class:`SessionTimeout`),
        without raising it.  Can be inspected at any time, including after
        :meth:`wait_closed` returns.
        """
        ...

    @property
    def max_datagram_size(self) -> int:
        """Maximum payload size for :meth:`send_datagram`."""
        ...

    @property
    def remote_address(self) -> tuple[str, int]:
        """The remote peer's ``(host, port)``."""
        ...

    @property
    def rtt(self) -> float:
        """Current estimated round-trip time in seconds."""
        ...

class SendStream:
    """A writable QUIC stream.

    Use as an async context manager: on clean exit, waits for any in-progress
    write to complete and then calls :meth:`finish`.  On exception, cancels
    any in-progress write immediately and calls ``reset(error_code=0)``::

        async with send:
            await send.write(b"hello")
    """

    async def __aenter__(self) -> SendStream: ...
    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: TracebackType | None,
    ) -> None: ...
    async def write(self, data: bytes) -> None:
        """Write all data to the stream.

        Blocks until all bytes have been accepted by the QUIC send buffer,
        but does **not** wait for the data to be transmitted on the wire or
        acknowledged by the peer.  If the connection is closed abruptly
        after this method returns, the peer may not receive the data.
        To ensure the peer has received everything, read an application-level
        acknowledgement.

        If :meth:`reset` is called while this write is pending, the write
        is interrupted and raises :class:`StreamClosedLocally`.

        Args:
            data: Bytes to write. Guaranteed to be fully written or raise.

        Raises:
            StreamClosedByPeer: If the peer sent ``STOP_SENDING`` on this
                stream (``kind="stop"``).
            StreamClosedLocally: If the stream was already finished or reset
                locally, or :meth:`reset` was called while the write was
                in progress.
            SessionClosedByPeer: If the peer closed the session.
            SessionClosedLocally: If the session was already closed locally.
            SessionTimeout: If the session timed out.
            ProtocolError: If the peer sent ``STOP_SENDING`` with an invalid
                error code, or another QUIC/HTTP/3 protocol violation occurred.
        """
        ...

    async def write_some(self, data: bytes) -> int:
        """Write some data, returning the number of bytes written.

        Low-level API. May write fewer bytes than ``len(data)`` due to flow
        control. Prefer :meth:`write` unless you need fine-grained control.
        Like :meth:`write`, this method only waits for the data to be
        accepted by the QUIC send buffer — it does not wait for
        transmission or acknowledgement by the peer.

        If :meth:`reset` is called while this write is pending, the write
        is interrupted and raises :class:`StreamClosedLocally`.

        Args:
            data: Bytes to write.

        Returns:
            Number of bytes actually written.

        Raises:
            StreamClosedByPeer: If the peer sent ``STOP_SENDING`` on this
                stream (``kind="stop"``).
            StreamClosedLocally: If the stream was already finished or reset
                locally, or :meth:`reset` was called while the write was
                in progress.
            SessionClosedByPeer: If the peer closed the session.
            SessionClosedLocally: If the session was already closed locally.
            SessionTimeout: If the session timed out.
            ProtocolError: If the peer sent ``STOP_SENDING`` with an invalid
                error code, or another QUIC/HTTP/3 protocol violation occurred.
        """
        ...

    async def finish(self) -> None:
        """Gracefully close the send side of the stream.

        Waits for any in-progress write to complete, then queues a QUIC
        ``FIN`` for sending.  Like :meth:`write`, this method does not
        wait for the ``FIN`` to be transmitted or acknowledged — if the
        connection is closed abruptly after this returns, the peer may
        not see the end-of-stream.  If the connection has already been
        lost, the frame is silently dropped (no error is raised).

        To wait for the peer to receive the data, call
        :meth:`wait_closed` after ``finish()``.  It returns ``None``
        once the peer has acknowledged receipt of all stream data
        (although not necessarily the processing of it).

        For a variety of reasons, the peer may not send acknowledgements
        immediately upon receiving data. As such, relying on `wait_closed`
        to know when the peer has read a stream to completion may introduce
        more latency than using an application-level response of some sort.

        Can be interrupted by :meth:`reset`, which causes both the pending
        write and this finish to raise :class:`StreamClosedLocally`.

        Raises:
            StreamClosedLocally: If the stream was already finished or reset.
        """
        ...

    def reset(self, error_code: int = 0) -> None:
        """Abruptly reset the stream.

        Discards any unsent data and queues a QUIC ``RESET_STREAM`` frame
        for sending.  The frame is transmitted on the next I/O cycle — this
        method does not wait for it to reach the peer.  If the connection
        has already been lost, the frame is silently dropped (no error is
        raised).

        If a :meth:`write` or :meth:`finish` is in progress, it will be
        interrupted and raise :class:`StreamClosedLocally`.

        Args:
            error_code: Application error code (default ``0``).

        Raises:
            StreamClosedLocally: If the stream was already finished or reset.
        """
        ...

    async def wait_closed(self) -> int | None:
        """Wait until the peer stops the stream or reads it to completion.

        Completes when the peer sends ``STOP_SENDING`` (via
        :meth:`RecvStream.stop`) **or** when the peer acknowledges receipt
        of all stream data after :meth:`finish`.

        A common pattern is to call this after :meth:`finish` to confirm
        the peer has received all data::

            await send.finish()
            await send.wait_closed()  # peer acknowledged receipt

        For a variety of reasons, the peer may not send acknowledgements
        immediately upon receiving data. As such, relying on `wait_closed`
        to know when the peer has read a stream to completion may introduce
        more latency than using an application-level response of some sort.

        Can be interrupted by :meth:`reset`, which causes
        ``wait_closed()`` to raise :class:`StreamClosedLocally`.

        Returns:
            The peer's error code if the peer sent ``STOP_SENDING``, or
            ``None`` if the stream was finished and the peer acknowledged
            receipt.  Also returns ``None`` if the ``STOP_SENDING`` error
            code is not a valid WebTransport error code.

        Raises:
            StreamClosedLocally: If the stream was already reset locally,
                or :meth:`reset` was called while waiting.
            SessionClosedByPeer: If the peer closed the session.
            SessionClosedLocally: If the session was already closed locally.
            SessionTimeout: If the session timed out.
            ProtocolError: If a QUIC or HTTP/3 protocol violation occurred.
        """
        ...

    @property
    def priority(self) -> int:
        """Stream scheduling priority (higher = higher priority)."""
        ...

    @priority.setter
    def priority(self, value: int) -> None: ...

class RecvStream:
    """A readable QUIC stream.

    Use as an async context manager: on clean exit, calls ``stop(error_code=0)``
    if the stream has not reached EOF (to promptly signal the peer).  On
    exception, cancels any in-progress read immediately and calls
    ``stop(error_code=0)``.

    Supports ``async for`` to iterate over incoming chunks until EOF::

        async for chunk in recv:
            process(chunk)
    """

    async def __aenter__(self) -> RecvStream: ...
    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: TracebackType | None,
    ) -> None: ...
    def __aiter__(self) -> AsyncIterator[bytes]: ...
    async def __anext__(self) -> bytes: ...
    async def read(self, n: int = -1, *, limit: int | None = None) -> bytes:
        """Read up to *n* bytes from the stream, or until EOF if *n* is ``-1``.

        Returns an empty ``bytes`` at EOF.  Like :meth:`asyncio.StreamReader.read`,
        this is idempotent: once EOF has been reached, subsequent calls keep
        returning ``b""`` without contacting the peer.

        If :meth:`stop` is called while a read is pending, the read is
        interrupted and raises :class:`StreamClosedLocally`.

        Args:
            n: Maximum number of bytes to read. Pass ``-1`` (the default)
                to read until EOF.
            limit: Maximum number of bytes to buffer when reading until EOF
                (``n=-1``).  Raises :class:`StreamTooLongError` if the stream
                exceeds this before finishing.  ``None`` (the default) means
                no limit.  Ignored when *n* >= 0.

        Raises:
            StreamClosedByPeer: If the peer sent ``RESET_STREAM`` on this
                stream (``kind="reset"``).
            StreamClosedLocally: If the stream was already stopped locally,
                or :meth:`stop` was called while the read was in progress.
            StreamTooLongError: If reading until EOF (``n=-1``) and the
                incoming data exceeds *limit*.  The exception's ``.limit``
                attribute contains the value that was exceeded.
            SessionClosedByPeer: If the peer closed the session.
            SessionClosedLocally: If the session was already closed locally.
            SessionTimeout: If the session timed out.
            ProtocolError: If the peer sent ``RESET_STREAM`` with an invalid
                error code, or another QUIC/HTTP/3 protocol violation occurred.
        """
        ...

    async def readexactly(self, n: int) -> bytes:
        """Read exactly *n* bytes.

        If EOF has already been reached, immediately raises
        :class:`StreamIncompleteReadError` with ``partial=b""``.

        If :meth:`stop` is called while a read is pending, the read is
        interrupted and raises :class:`StreamClosedLocally`.

        Args:
            n: Number of bytes to read.

        Returns:
            Exactly *n* bytes.

        Raises:
            StreamIncompleteReadError: If EOF is reached before *n* bytes
                have been received. The ``.expected`` attribute contains the
                requested count and ``.partial`` contains the bytes that were
                successfully read before EOF.
            StreamClosedByPeer: If the peer sent ``RESET_STREAM`` on this
                stream (``kind="reset"``).
            StreamClosedLocally: If the stream was already stopped locally,
                or :meth:`stop` was called while the read was in progress.
            SessionClosedByPeer: If the peer closed the session.
            SessionClosedLocally: If the session was already closed locally.
            SessionTimeout: If the session timed out.
            ProtocolError: If the peer sent ``RESET_STREAM`` with an invalid
                error code, or another QUIC/HTTP/3 protocol violation occurred.
        """
        ...

    def stop(self, error_code: int = 0) -> None:
        """Tell the peer to stop sending on this stream.

        Queues a QUIC ``STOP_SENDING`` frame for sending.  The frame is
        transmitted on the next I/O cycle — this method does not wait for
        it to reach the peer.  If the connection has already been lost,
        the frame is silently dropped (no error is raised).

        If a :meth:`read` or :meth:`readexactly` call is in progress, it
        will be interrupted and raise :class:`StreamClosedLocally`.

        Args:
            error_code: Application error code (default ``0``).

        Raises:
            StreamClosedLocally: If the stream was already stopped.
        """
        ...

    async def wait_closed(self) -> int | None:
        """Wait until the peer resets the stream or it is otherwise closed.

        Completes when the peer sends ``RESET_STREAM`` **or** when the
        stream was previously :meth:`stop`\\ ped locally, **or** when the
        peer calls :meth:`~SendStream.finish` and all data has been
        received, after which it is no longer meaningful for the stream
        to be reset.

        For a variety of reasons, the peer may not send acknowledgements
        immediately upon receiving data. As such, relying on ``wait_closed``
        to know when the peer has finished a stream may introduce more
        latency than using an application-level response of some sort.

        Can be interrupted by :meth:`stop`, which causes
        ``wait_closed()`` to raise :class:`StreamClosedLocally`.

        Returns:
            The peer's error code if the peer sent ``RESET_STREAM``, or
            ``None`` if the stream was stopped locally or the peer
            finished the stream and all data was received.

        Raises:
            StreamClosedLocally: If the stream was already stopped locally,
                or :meth:`stop` was called while waiting.
            SessionClosedByPeer: If the peer closed the session.
            SessionClosedLocally: If the session was already closed locally.
            SessionTimeout: If the session timed out.
            ProtocolError: If a QUIC or HTTP/3 protocol violation occurred.
        """
        ...
