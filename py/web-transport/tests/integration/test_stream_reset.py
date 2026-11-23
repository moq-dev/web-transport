"""Integration tests for stream reset, stop, cancellation, and half-closed states."""

import asyncio

import pytest

import web_transport

from .conftest import suspended

# Exceeds the peer's stream flow-control window, so writing it blocks until read.
UNBOUNDED = b"x" * (16 * 1024 * 1024)


@pytest.mark.asyncio
async def test_send_reset_with_code(session_pair):
    """send.reset(42) -> peer read() raises StreamClosedByPeer(reset, 42)."""
    server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()

    accepted = asyncio.Event()

    async def server_side():
        _send_s, recv_s = await server_session.accept_bi()
        accepted.set()
        with pytest.raises(web_transport.StreamClosedByPeer) as exc_info:
            await recv_s.read()
        assert exc_info.value.kind == "reset"
        assert exc_info.value.code == 42

    task = asyncio.create_task(server_side())
    # A reset before the peer accepts discards the stream header (see note below).
    await asyncio.wait_for(accepted.wait(), timeout=5.0)
    send.reset(42)
    await asyncio.wait_for(task, timeout=5.0)


@pytest.mark.asyncio
async def test_send_reset_default_code(session_pair):
    """send.reset() -> code=0."""
    server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()

    accepted = asyncio.Event()

    async def server_side():
        _send_s, recv_s = await server_session.accept_bi()
        accepted.set()
        with pytest.raises(web_transport.StreamClosedByPeer) as exc_info:
            await recv_s.read()
        assert exc_info.value.kind == "reset"
        assert exc_info.value.code == 0

    task = asyncio.create_task(server_side())
    await asyncio.wait_for(accepted.wait(), timeout=5.0)
    send.reset()
    await asyncio.wait_for(task, timeout=5.0)


@pytest.mark.asyncio
async def test_double_reset_raises(session_pair):
    """reset() twice -> StreamClosedLocally."""
    _server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()
    send.reset()
    with pytest.raises(web_transport.StreamClosedLocally):
        send.reset()


@pytest.mark.asyncio
async def test_write_after_reset_raises(session_pair):
    """reset() -> write() -> StreamClosedLocally."""
    _server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()
    send.reset()
    with pytest.raises(web_transport.StreamClosedLocally):
        await send.write(b"x")


@pytest.mark.asyncio
async def test_write_after_finish_raises(session_pair):
    """finish() -> write() -> StreamClosedLocally."""
    _server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()
    await send.finish()
    with pytest.raises(web_transport.StreamClosedLocally):
        await send.write(b"x")


@pytest.mark.asyncio
async def test_recv_stop_with_code(session_pair):
    """recv.stop(42) -> peer write() raises StreamClosedByPeer(stop, 42)."""
    server_session, client_session = session_pair

    send, recv = await client_session.open_bi()

    async def server_side():
        send_s, _recv_s = await server_session.accept_bi()
        assert await send_s.wait_closed() == 42
        with pytest.raises(web_transport.StreamClosedByPeer) as exc_info:
            await send_s.write(b"x")
        assert exc_info.value.kind == "stop"
        assert exc_info.value.code == 42

    task = asyncio.create_task(server_side())
    recv.stop(42)
    await asyncio.wait_for(task, timeout=5.0)


@pytest.mark.asyncio
async def test_recv_stop_default_code(session_pair):
    """recv.stop() -> code=0."""
    server_session, client_session = session_pair

    send, recv = await client_session.open_bi()

    async def server_side():
        send_s, _recv_s = await server_session.accept_bi()
        assert await send_s.wait_closed() == 0
        with pytest.raises(web_transport.StreamClosedByPeer) as exc_info:
            await send_s.write(b"x")
        assert exc_info.value.kind == "stop"
        assert exc_info.value.code == 0

    task = asyncio.create_task(server_side())
    recv.stop()
    await asyncio.wait_for(task, timeout=5.0)


@pytest.mark.asyncio
async def test_double_stop_raises(session_pair):
    """stop() twice -> StreamClosedLocally."""
    _server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()
    recv.stop()
    with pytest.raises(web_transport.StreamClosedLocally):
        recv.stop()


@pytest.mark.asyncio
async def test_read_after_stop_raises(session_pair):
    """stop() -> read() -> StreamClosedLocally."""
    _server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()
    recv.stop()
    with pytest.raises(web_transport.StreamClosedLocally):
        await recv.read()


@pytest.mark.asyncio
async def test_reset_cancels_pending_write(session_pair):
    """Start write() task, call reset() -> write raises StreamClosedLocally."""
    server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()

    async def write_large():
        with pytest.raises(web_transport.StreamClosedLocally):
            await send.write(UNBOUNDED)

    async def peer_reads():
        send_s, recv_s = await server_session.accept_bi()
        assert await recv_s.read(1) == b"x"
        return send_s, recv_s  # kept open, so the peer never stops the stream

    write_task = asyncio.create_task(write_large())
    # Once the peer sees data the write is in flight, and flow control blocks it.
    peer = asyncio.create_task(peer_reads())
    done, _ = await asyncio.wait(
        {write_task, peer}, timeout=5.0, return_when=asyncio.FIRST_COMPLETED
    )
    if write_task in done:
        write_task.result()  # raises the writer's failure
        pytest.fail("write finished before the peer read it")
    assert peer in done, "peer never read the write"
    _streams = peer.result()
    send.reset()
    await asyncio.wait_for(write_task, timeout=5.0)


@pytest.mark.asyncio
async def test_stop_cancels_pending_read(session_pair):
    """Start read() on idle stream, call stop() -> StreamClosedLocally."""
    server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()

    # Accept the stream on the server side but don't write anything
    _send_s, _recv_s = await server_session.accept_bi()

    async def read_pending():
        with pytest.raises(web_transport.StreamClosedLocally):
            await recv.read()

    read_task = asyncio.create_task(read_pending())
    await suspended(read_task)
    recv.stop()
    await asyncio.wait_for(read_task, timeout=5.0)


# NOTE: There is no `test_send_context_manager_resets_on_exception` test.
#
# `async with send: raise` resets the stream (RESET_STREAM) before any data is
# written. The WebTransport stream header (signal value + session ID) is still
# buffered at that point, and an unreliable RESET_STREAM discards it — so the
# peer can never associate the stream with a session and accept_bi() hangs.
#
# draft-ietf-webtrans-http3 §4.4 fixes this by mandating RESET_STREAM_AT with a
# Reliable Size of at least the header length, but quinn 0.11 does not implement
# RESET_STREAM_AT. Until it does, this case is inherently racy and untestable.


@pytest.mark.asyncio
async def test_recv_context_manager_stops_on_exception(session_pair):
    """async with recv: raise -> peer's write raises StreamClosedByPeer(stop)."""
    server_session, client_session = session_pair

    send, recv = await client_session.open_bi()

    async def server_side():
        send_s, _recv_s = await server_session.accept_bi()
        assert await send_s.wait_closed() == 0
        with pytest.raises(web_transport.StreamClosedByPeer) as exc_info:
            await send_s.write(b"x")
        assert exc_info.value.kind == "stop"
        assert exc_info.value.code == 0

    task = asyncio.create_task(server_side())

    with pytest.raises(RuntimeError, match="intentional"):
        async with recv:
            raise RuntimeError("intentional")

    await asyncio.wait_for(task, timeout=5.0)


@pytest.mark.asyncio
async def test_recv_context_manager_stops_if_not_at_eof(session_pair):
    """Clean exit before EOF -> peer's write raises StreamClosedByPeer(stop)."""
    server_session, client_session = session_pair

    send, recv = await client_session.open_bi()

    async def server_side():
        send_s, _recv_s = await server_session.accept_bi()
        # Write some data but don't finish
        await send_s.write(b"hello")
        assert await send_s.wait_closed() == 0
        with pytest.raises(web_transport.StreamClosedByPeer) as exc_info:
            await send_s.write(b"x")
        assert exc_info.value.kind == "stop"
        assert exc_info.value.code == 0

    task = asyncio.create_task(server_side())

    async with recv:
        # Read some data but exit before EOF
        data = await recv.read(5)
        assert data == b"hello"
        # Exit without reaching EOF -> stop() should be sent

    await asyncio.wait_for(task, timeout=5.0)


@pytest.mark.asyncio
async def test_recv_context_manager_no_stop_at_eof(session_pair):
    """Read all data then clean exit -> no stop sent."""
    server_session, client_session = session_pair

    send, recv = await client_session.open_bi()

    async def server_side():
        send_s, _recv_s = await server_session.accept_bi()
        async with send_s:
            await send_s.write(b"hello")
        # finish() was called via context manager, so client will see EOF
        # Verify the stream closed cleanly (no STOP_SENDING from peer)
        code = await send_s.wait_closed()
        assert code is None  # None = clean finish acknowledged, not a stop code

    task = asyncio.create_task(server_side())

    async with recv:
        data = await recv.read()
        assert data == b"hello"

    await asyncio.wait_for(task, timeout=5.0)


@pytest.mark.asyncio
async def test_half_closed_write_after_recv_eof(session_pair):
    """Peer finishes send -> our send still works on bidi stream."""
    server_session, client_session = session_pair

    send, recv = await client_session.open_bi()

    async def server_side():
        send_s, recv_s = await server_session.accept_bi()
        # Finish our send side
        await send_s.finish()
        # Read what the client sends after our EOF
        data = await recv_s.read()
        return data

    task = asyncio.create_task(server_side())

    # Wait for peer's FIN
    data = await recv.read()
    assert data == b""  # EOF

    # Our send side should still work
    async with send:
        await send.write(b"still works")

    result = await asyncio.wait_for(task, timeout=5.0)
    assert result == b"still works"


@pytest.mark.asyncio
async def test_half_closed_read_after_send_finish(session_pair):
    """We finish send -> our recv still works on bidi stream."""
    server_session, client_session = session_pair

    send, recv = await client_session.open_bi()

    async def server_side():
        send_s, recv_s = await server_session.accept_bi()
        # Wait for client's FIN
        data = await recv_s.read()
        assert data == b"hello"
        # Send our response
        async with send_s:
            await send_s.write(b"response")

    task = asyncio.create_task(server_side())

    # Finish our send side
    async with send:
        await send.write(b"hello")

    # Our recv side should still work
    data = await recv.read()
    assert data == b"response"

    await task


@pytest.mark.asyncio
async def test_peer_reset_during_read(session_pair):
    """Peer reset(7) while we await recv.read() -> StreamClosedByPeer."""
    server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()
    send_s, _recv_s = await server_session.accept_bi()

    read_task = asyncio.create_task(recv.read())
    await suspended(read_task)
    send_s.reset(7)

    with pytest.raises(web_transport.StreamClosedByPeer) as exc_info:
        await asyncio.wait_for(read_task, timeout=5.0)

    assert exc_info.value.kind == "reset"
    assert exc_info.value.code == 7


@pytest.mark.asyncio
async def test_uni_send_reset(session_pair):
    """Uni stream reset -> peer sees StreamClosedByPeer(reset)."""
    server_session, client_session = session_pair

    send = await client_session.open_uni()

    accepted = asyncio.Event()

    async def server_side():
        recv = await server_session.accept_uni()
        accepted.set()
        with pytest.raises(web_transport.StreamClosedByPeer) as exc_info:
            await recv.read()
        assert exc_info.value.kind == "reset"
        assert exc_info.value.code == 0

    task = asyncio.create_task(server_side())
    await asyncio.wait_for(accepted.wait(), timeout=5.0)
    send.reset()
    await asyncio.wait_for(task, timeout=5.0)


@pytest.mark.asyncio
async def test_uni_recv_stop(session_pair):
    """Uni stream stop -> peer write raises StreamClosedByPeer(stop)."""
    server_session, client_session = session_pair

    send = await client_session.open_uni()

    async def server_side():
        recv = await server_session.accept_uni()
        recv.stop(42)

    task = asyncio.create_task(server_side())
    assert await asyncio.wait_for(send.wait_closed(), timeout=5.0) == 42

    with pytest.raises(web_transport.StreamClosedByPeer) as exc_info:
        await send.write(b"x")

    assert exc_info.value.kind == "stop"
    assert exc_info.value.code == 42
    await task


@pytest.mark.asyncio
async def test_readexactly_during_peer_reset(session_pair):
    """readexactly() interrupted by peer reset -> StreamClosedByPeer."""
    server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()
    send_s, _recv_s = await server_session.accept_bi()

    read_task = asyncio.create_task(recv.readexactly(1000))
    await suspended(read_task)
    send_s.reset(7)

    with pytest.raises(web_transport.StreamClosedByPeer) as exc_info:
        await asyncio.wait_for(read_task, timeout=5.0)

    assert exc_info.value.kind == "reset"
    assert exc_info.value.code == 7


@pytest.mark.asyncio
async def test_double_finish_raises(session_pair):
    """finish() after finish() -> StreamClosedLocally."""
    _server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()
    await send.finish()
    with pytest.raises(web_transport.StreamClosedLocally):
        await send.finish()


@pytest.mark.asyncio
async def test_finish_after_reset_raises(session_pair):
    """reset() then finish() -> StreamClosedLocally (cancellation token fires)."""
    _server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()
    send.reset()
    with pytest.raises(web_transport.StreamClosedLocally):
        await send.finish()


@pytest.mark.asyncio
async def test_stop_cancels_pending_iteration(session_pair):
    """stop() interrupts async for on recv -> StreamClosedLocally."""
    server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()

    # Accept stream but don't write anything — iterator will block
    _send_s, _recv_s = await server_session.accept_bi()

    async def iterate():
        with pytest.raises(web_transport.StreamClosedLocally):
            async for _chunk in recv:
                pass

    iter_task = asyncio.create_task(iterate())
    await suspended(iter_task)
    recv.stop()
    await asyncio.wait_for(iter_task, timeout=5.0)


@pytest.mark.asyncio
async def test_read_zero_after_stop_raises(session_pair):
    """read(0) after stop() -> StreamClosedLocally (cancellation fires even for n=0)."""
    _server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()
    recv.stop()

    # read(0) enters cancellable_read, where the biased select sees the
    # cancellation token first, so it raises StreamClosedLocally.
    with pytest.raises(web_transport.StreamClosedLocally):
        await recv.read(0)


@pytest.mark.asyncio
async def test_peer_session_close_during_pending_read(session_pair):
    """Peer closes session while recv.read() is awaited -> error or EOF."""
    server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()
    _send_s, _recv_s = await server_session.accept_bi()

    read_task = asyncio.create_task(recv.read())
    await suspended(read_task)
    server_session.close(1, "going away")
    await server_session.wait_closed()

    with pytest.raises(web_transport.SessionClosedByPeer) as exc_info:
        await asyncio.wait_for(read_task, timeout=5.0)
    assert exc_info.value.source == "session"
    assert exc_info.value.code == 1
    assert exc_info.value.reason == "going away"


@pytest.mark.asyncio
async def test_peer_session_close_during_pending_write(session_pair):
    """Peer closes session while send.write() is blocked -> error."""
    server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()

    write_task = asyncio.create_task(send.write(UNBOUNDED))
    # Once the peer sees data the write is in flight, and flow control blocks it.
    _send_s, recv_s = await server_session.accept_bi()
    assert await recv_s.read(1) == b"x"
    server_session.close(2, "closing")
    await server_session.wait_closed()

    with pytest.raises(web_transport.SessionClosedByPeer) as exc_info:
        await asyncio.wait_for(write_task, timeout=5.0)
    assert exc_info.value.source == "session"
    assert exc_info.value.code == 2
    assert exc_info.value.reason == "closing"


@pytest.mark.asyncio
async def test_stop_cancels_pending_readexactly(session_pair):
    """stop() interrupts pending readexactly() -> StreamClosedLocally."""
    server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()

    # Accept stream but don't write anything — readexactly will block
    _send_s, _recv_s = await server_session.accept_bi()

    async def read_pending():
        with pytest.raises(web_transport.StreamClosedLocally):
            await recv.readexactly(100)

    read_task = asyncio.create_task(read_pending())
    await suspended(read_task)
    recv.stop()
    await asyncio.wait_for(read_task, timeout=5.0)


@pytest.mark.asyncio
async def test_write_some_after_finish_raises(session_pair):
    """finish() then write_some() -> StreamClosedLocally."""
    _server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()
    await send.finish()
    with pytest.raises(web_transport.StreamClosedLocally):
        await send.write_some(b"x")


@pytest.mark.asyncio
async def test_write_some_after_reset_raises(session_pair):
    """reset() then write_some() -> StreamClosedLocally."""
    _server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()
    send.reset()
    with pytest.raises(web_transport.StreamClosedLocally):
        await send.write_some(b"x")


# ---------------------------------------------------------------------------
# SendStream.wait_closed tests
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_send_wait_closed_returns_stop_code(session_pair):
    """wait_closed() returns peer's error code from STOP_SENDING."""
    server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()

    async def server_side():
        _send_s, recv_s = await server_session.accept_bi()
        recv_s.stop(42)

    task = asyncio.create_task(server_side())
    code = await asyncio.wait_for(send.wait_closed(), timeout=5.0)
    assert code == 42
    await task


@pytest.mark.asyncio
async def test_send_wait_closed_after_reset_raises(session_pair):
    """reset() then wait_closed() -> StreamClosedLocally."""
    _server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()
    send.reset()
    with pytest.raises(web_transport.StreamClosedLocally):
        await send.wait_closed()


@pytest.mark.asyncio
async def test_reset_cancels_pending_send_wait_closed(session_pair):
    """reset() while wait_closed() is pending -> StreamClosedLocally."""
    server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()

    # Accept stream but don't send STOP_SENDING — wait_closed will block
    _send_s, _recv_s = await server_session.accept_bi()

    async def wait():
        with pytest.raises(web_transport.StreamClosedLocally):
            await send.wait_closed()

    wait_task = asyncio.create_task(wait())
    await suspended(wait_task)
    send.reset()
    await asyncio.wait_for(wait_task, timeout=5.0)


@pytest.mark.asyncio
async def test_send_finish_then_wait_closed_peer_acknowledges(session_pair):
    """finish() -> peer reads to completion -> wait_closed() returns None."""
    server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()

    async def server_side():
        _send_s, recv_s = await server_session.accept_bi()
        data = await recv_s.read()
        assert data == b"hello"

    task = asyncio.create_task(server_side())

    await send.write(b"hello")
    await send.finish()
    code = await asyncio.wait_for(send.wait_closed(), timeout=5.0)
    assert code is None
    await task


@pytest.mark.asyncio
async def test_send_wait_closed_session_closed_by_peer(session_pair):
    """Peer closes session while send.wait_closed() pending -> SessionClosedByPeer."""
    server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()
    _send_s, _recv_s = await server_session.accept_bi()

    wait_task = asyncio.create_task(send.wait_closed())
    await suspended(wait_task)
    server_session.close(0, "done")
    await server_session.wait_closed()

    with pytest.raises(web_transport.SessionClosedByPeer) as exc_info:
        await asyncio.wait_for(wait_task, timeout=5.0)
    assert exc_info.value.source == "session"
    assert exc_info.value.code == 0
    assert exc_info.value.reason == "done"


# ---------------------------------------------------------------------------
# RecvStream.wait_closed tests
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_recv_wait_closed_returns_reset_code(session_pair):
    """Peer resets stream -> recv.wait_closed() returns error code."""
    server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()

    async def server_side():
        send_s, _recv_s = await server_session.accept_bi()
        send_s.reset(42)

    task = asyncio.create_task(server_side())
    code = await asyncio.wait_for(recv.wait_closed(), timeout=5.0)
    assert code == 42
    await task


@pytest.mark.asyncio
async def test_recv_wait_closed_after_stop_raises(session_pair):
    """stop() then recv.wait_closed() -> StreamClosedLocally."""
    _server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()
    recv.stop()
    with pytest.raises(web_transport.StreamClosedLocally):
        await recv.wait_closed()


@pytest.mark.asyncio
async def test_stop_cancels_pending_recv_wait_closed(session_pair):
    """stop() while recv.wait_closed() is pending -> StreamClosedLocally."""
    server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()

    _send_s, _recv_s = await server_session.accept_bi()

    async def wait():
        with pytest.raises(web_transport.StreamClosedLocally):
            await recv.wait_closed()

    wait_task = asyncio.create_task(wait())
    await suspended(wait_task)
    recv.stop()
    await asyncio.wait_for(wait_task, timeout=5.0)


@pytest.mark.asyncio
async def test_recv_wait_closed_peer_finishes(session_pair):
    """Peer finishes stream -> recv.wait_closed() returns None."""
    server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()

    async def server_side():
        send_s, _recv_s = await server_session.accept_bi()
        await send_s.write(b"hello")
        await send_s.finish()

    task = asyncio.create_task(server_side())

    # Read the data so the stream can complete
    data = await recv.read()
    assert data == b"hello"

    code = await asyncio.wait_for(recv.wait_closed(), timeout=5.0)
    assert code is None
    await task


@pytest.mark.asyncio
async def test_recv_wait_closed_session_closed_by_peer(session_pair):
    """Peer closes session while recv.wait_closed() pending -> SessionClosedByPeer."""
    server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()
    _send_s, _recv_s = await server_session.accept_bi()

    wait_task = asyncio.create_task(recv.wait_closed())
    await suspended(wait_task)
    server_session.close(0, "done")
    await server_session.wait_closed()

    with pytest.raises(web_transport.SessionClosedByPeer) as exc_info:
        await asyncio.wait_for(wait_task, timeout=5.0)
    assert exc_info.value.source == "session"
    assert exc_info.value.code == 0
    assert exc_info.value.reason == "done"


# --- asyncio task cancellation tests ---
#
# The server makes no further progress until the cancelled call has returned,
# so a cancellation that waited on the peer would hang until the guard fires.


@pytest.mark.asyncio
async def test_asyncio_cancel_pending_read(session_pair):
    """task.cancel() on a pending read() raises CancelledError immediately."""
    server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()
    send_s, _recv_s = await server_session.accept_bi()
    # Send a small chunk so the read starts, then hold the stream open
    await send_s.write(b"partial")

    # read() with no size reads until EOF — it will receive "partial" but
    # keep waiting because the server hasn't finished the stream yet.
    read_fut = asyncio.ensure_future(recv.read())
    await suspended(read_fut)

    read_fut.cancel()
    with pytest.raises(asyncio.CancelledError):
        await asyncio.wait_for(read_fut, timeout=5.0)


@pytest.mark.asyncio
async def test_asyncio_cancel_pending_readexactly(session_pair):
    """task.cancel() on a pending readexactly() raises CancelledError immediately."""
    server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()
    send_s, _recv_s = await server_session.accept_bi()
    # Send fewer bytes than requested so readexactly blocks mid-read
    await send_s.write(b"short")

    read_fut = asyncio.ensure_future(recv.readexactly(100))
    await suspended(read_fut)

    read_fut.cancel()
    with pytest.raises(asyncio.CancelledError):
        await asyncio.wait_for(read_fut, timeout=5.0)


@pytest.mark.asyncio
async def test_asyncio_cancel_read_then_read_again(session_pair):
    """After asyncio cancellation of read(), the stream is still usable."""
    server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()
    send_s, _recv_s = await server_session.accept_bi()
    # Send a partial chunk so the read is in progress, then hold open
    await send_s.write(b"first-chunk")

    # First read — cancel it via asyncio while it's mid-stream
    read_fut = asyncio.ensure_future(recv.read())
    await suspended(read_fut)

    read_fut.cancel()
    with pytest.raises(asyncio.CancelledError):
        await asyncio.wait_for(read_fut, timeout=5.0)

    await send_s.write(b"after-cancel")
    await send_s.finish()

    # Second read — should succeed because no STOP_SENDING was sent.
    # The cancelled read may have consumed "first-chunk", so we just
    # verify the stream is still functional and eventually reaches EOF.
    assert b"after-cancel" in await asyncio.wait_for(recv.read(), timeout=5.0)


@pytest.mark.asyncio
async def test_asyncio_cancel_readexactly_then_read_again(session_pair):
    """After asyncio cancellation of readexactly(), the stream is still usable."""
    server_session, client_session = session_pair

    _send, recv = await client_session.open_bi()
    send_s, _recv_s = await server_session.accept_bi()
    # Send fewer bytes than readexactly requests, so it blocks mid-read
    await send_s.write(b"short")

    # First readexactly — cancel it via asyncio while it's mid-read
    read_fut = asyncio.ensure_future(recv.readexactly(100))
    await suspended(read_fut)

    read_fut.cancel()
    with pytest.raises(asyncio.CancelledError):
        await asyncio.wait_for(read_fut, timeout=5.0)

    await send_s.write(b"hello")
    await send_s.finish()

    # Stream should still work — read remaining data
    assert b"hello" in await asyncio.wait_for(recv.read(), timeout=5.0)


@pytest.mark.asyncio
async def test_asyncio_cancel_pending_write(session_pair):
    """task.cancel() on a pending write() raises CancelledError immediately."""
    server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()

    write_task = asyncio.ensure_future(send.write(UNBOUNDED))
    # Once the peer sees data the write is in flight, and flow control blocks it.
    _send_s, recv_s = await server_session.accept_bi()
    assert await recv_s.read(1) == b"x"

    write_task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await asyncio.wait_for(write_task, timeout=5.0)
