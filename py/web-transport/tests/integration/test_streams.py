"""Integration tests for stream read/write operations."""

import asyncio

import pytest

import web_transport


@pytest.mark.asyncio
async def test_write_some(session_pair):
    server_session, client_session = session_pair

    async def server_side():
        send, recv = await server_session.accept_bi()
        data = await recv.read()
        async with send:
            await send.write(data)

    server_task = asyncio.create_task(server_side())

    send, recv = await client_session.open_bi()
    n = await send.write_some(b"hello")
    assert isinstance(n, int)
    assert 0 < n <= len(b"hello")
    await send.finish()

    response = await recv.read()
    assert b"hello".startswith(response)
    await server_task


@pytest.mark.asyncio
async def test_send_stream_context_manager_finish(session_pair):
    server_session, client_session = session_pair

    async def server_side():
        _send_s, recv_s = await server_session.accept_bi()
        data = await recv_s.read()
        assert data == b"test data"

    task = asyncio.create_task(server_side())

    send, _recv = await client_session.open_bi()
    async with send:
        await send.write(b"test data")
    # After clean exit, stream should be finished — verify peer sees FIN + data
    await asyncio.wait_for(task, timeout=5.0)


@pytest.mark.asyncio
async def test_recv_stream_async_iteration(session_pair):
    server_session, client_session = session_pair

    async def server_side():
        send, recv = await server_session.accept_bi()
        chunks = []
        async for chunk in recv:
            chunks.append(chunk)
        async with send:
            await send.write(b"".join(chunks))

    send, recv = await client_session.open_bi()

    server_task = asyncio.create_task(server_side())

    async with send:
        await send.write(b"chunk1")
        await send.write(b"chunk2")

    response = await recv.read()
    assert response == b"chunk1chunk2"
    await server_task


@pytest.mark.asyncio
async def test_readexactly(session_pair):
    server_session, client_session = session_pair

    async def server_side():
        send, _recv = await server_session.accept_bi()
        async with send:
            await send.write(b"exactly10!")

    _send, recv = await client_session.open_bi()
    server_task = asyncio.create_task(server_side())

    data = await recv.readexactly(10)
    assert data == b"exactly10!"
    await server_task


@pytest.mark.asyncio
async def test_readexactly_incomplete(session_pair):
    server_session, client_session = session_pair

    async def server_side():
        send, _recv = await server_session.accept_bi()
        async with send:
            await send.write(b"short")

    _send, recv = await client_session.open_bi()
    server_task = asyncio.create_task(server_side())

    with pytest.raises(web_transport.StreamIncompleteReadError) as exc_info:
        await recv.readexactly(100)

    assert exc_info.value.expected == 100
    assert exc_info.value.partial == b"short"
    await server_task


@pytest.mark.asyncio
async def test_priority(session_pair):
    _server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()
    # Default priority
    p = send.priority
    assert isinstance(p, int)

    send.priority = 42
    assert send.priority == 42
    await send.finish()


@pytest.mark.asyncio
async def test_read_n_bytes(session_pair):
    """read(5) returns up to 5 bytes from longer stream."""
    server_session, client_session = session_pair

    async def server_side():
        send, _recv = await server_session.accept_bi()
        async with send:
            await send.write(b"hello world")

    _send, recv = await client_session.open_bi()
    task = asyncio.create_task(server_side())

    data = await recv.read(5)
    assert 0 < len(data) <= 5
    assert b"hello world".startswith(data)
    await task


@pytest.mark.asyncio
async def test_read_n_returns_less_at_eof(session_pair):
    """read(100) on short stream returns available bytes."""
    server_session, client_session = session_pair

    async def server_side():
        send, _recv = await server_session.accept_bi()
        async with send:
            await send.write(b"hi")

    _send, recv = await client_session.open_bi()
    task = asyncio.create_task(server_side())

    # Read all chunks until empty
    chunks = []
    while True:
        data = await recv.read(100)
        if not data:
            break
        chunks.append(data)

    assert b"".join(chunks) == b"hi"
    await task


@pytest.mark.asyncio
async def test_read_zero_returns_empty(session_pair):
    """read(0) returns b"" immediately."""
    _server_session, client_session = session_pair

    send, recv = await client_session.open_bi()
    data = await recv.read(0)
    assert data == b""
    await send.finish()


@pytest.mark.asyncio
async def test_read_all_to_eof(session_pair):
    """read() (default n=-1) reads until EOF."""
    server_session, client_session = session_pair

    async def server_side():
        send, _recv = await server_session.accept_bi()
        async with send:
            await send.write(b"all the data")

    _send, recv = await client_session.open_bi()
    task = asyncio.create_task(server_side())

    data = await recv.read()
    assert data == b"all the data"
    await task


@pytest.mark.asyncio
async def test_read_with_limit(session_pair):
    """read(limit=10) on 20-byte stream -> StreamTooLongError."""
    server_session, client_session = session_pair

    async def server_side():
        send, _recv = await server_session.accept_bi()
        async with send:
            await send.write(b"x" * 20)

    _send, recv = await client_session.open_bi()
    task = asyncio.create_task(server_side())

    with pytest.raises(web_transport.StreamTooLongError) as exc_info:
        await recv.read(limit=10)

    assert exc_info.value.limit == 10
    await task


@pytest.mark.asyncio
async def test_read_after_eof_returns_empty(session_pair):
    """After EOF, read() returns b"" (idempotent)."""
    server_session, client_session = session_pair

    async def server_side():
        send, _recv = await server_session.accept_bi()
        async with send:
            await send.write(b"data")

    _send, recv = await client_session.open_bi()
    task = asyncio.create_task(server_side())

    # Read until EOF
    data = await recv.read()
    assert data == b"data"

    # Read again after EOF
    data2 = await recv.read()
    assert data2 == b""

    # And again
    data3 = await recv.read()
    assert data3 == b""

    await task


@pytest.mark.asyncio
async def test_read_n_after_eof_returns_empty(session_pair):
    """read(5) after EOF returns b"" (idempotent for n>0)."""
    server_session, client_session = session_pair

    async def server_side():
        send, _recv = await server_session.accept_bi()
        async with send:
            await send.write(b"data")

    _send, recv = await client_session.open_bi()
    task = asyncio.create_task(server_side())

    # Read until EOF
    data = await recv.read()
    assert data == b"data"

    # read(5) after EOF should return b""
    data2 = await recv.read(5)
    assert data2 == b""

    await task


@pytest.mark.asyncio
async def test_readexactly_zero(session_pair):
    """readexactly(0) returns b""."""
    _server_session, client_session = session_pair

    send, recv = await client_session.open_bi()
    data = await recv.readexactly(0)
    assert data == b""
    await send.finish()


@pytest.mark.asyncio
async def test_readexactly_after_eof(session_pair):
    """readexactly(5) after EOF -> StreamIncompleteReadError."""
    server_session, client_session = session_pair

    async def server_side():
        send, _recv = await server_session.accept_bi()
        await send.finish()  # Immediately EOF

    _send, recv = await client_session.open_bi()
    task = asyncio.create_task(server_side())

    # Wait for EOF
    data = await recv.read()
    assert data == b""

    with pytest.raises(web_transport.StreamIncompleteReadError) as exc_info:
        await recv.readexactly(5)

    assert exc_info.value.expected == 5
    assert exc_info.value.partial == b""
    await task


@pytest.mark.asyncio
async def test_write_explicit(session_pair):
    """write(data) writes all bytes (verify roundtrip)."""
    server_session, client_session = session_pair

    payload = b"explicit write test data"

    async def server_side():
        send, recv = await server_session.accept_bi()
        data = await recv.read()
        async with send:
            await send.write(data)

    task = asyncio.create_task(server_side())

    send, recv = await client_session.open_bi()
    await send.write(payload)
    await send.finish()

    response = await recv.read()
    assert response == payload
    await task


@pytest.mark.asyncio
async def test_finish_explicit(session_pair):
    """finish() signals EOF to peer."""
    server_session, client_session = session_pair

    async def server_side():
        _send, recv = await server_session.accept_bi()
        data = await recv.read()
        assert data == b""  # Only got EOF, no data

    send, _recv = await client_session.open_bi()
    task = asyncio.create_task(server_side())

    await send.finish()  # Send EOF immediately
    await asyncio.wait_for(task, timeout=5.0)


@pytest.mark.asyncio
async def test_large_transfer(session_pair):
    """Write 1 MB -> recv reads all 1 MB correctly."""
    server_session, client_session = session_pair

    size = 1024 * 1024  # 1 MB
    payload = bytes(range(256)) * (size // 256)

    async def server_side():
        send, recv = await server_session.accept_bi()
        data = await recv.read()
        async with send:
            await send.write(data)

    task = asyncio.create_task(server_side())

    send, recv = await client_session.open_bi()
    async with send:
        await send.write(payload)

    response = await recv.read()
    assert len(response) == len(payload)
    assert response == payload
    await task


@pytest.mark.asyncio
async def test_empty_write(session_pair):
    """write(b"") succeeds without error and peer sees EOF with no data."""
    server_session, client_session = session_pair

    async def server_side():
        _send_s, recv_s = await server_session.accept_bi()
        data = await recv_s.read()
        assert data == b""

    task = asyncio.create_task(server_side())

    send, _recv = await client_session.open_bi()
    await send.write(b"")
    await send.finish()
    await asyncio.wait_for(task, timeout=5.0)


@pytest.mark.asyncio
async def test_concurrent_bidi_io(session_pair):
    """Simultaneous send.write() and recv.read() on same bidi stream."""
    server_session, client_session = session_pair

    async def server_side():
        send_s, recv_s = await server_session.accept_bi()
        # Echo: read all then write all
        data = await recv_s.read()
        async with send_s:
            await send_s.write(data)

    server_task = asyncio.create_task(server_side())

    send, recv = await client_session.open_bi()

    # Run write and read concurrently on the same stream pair
    async def do_write():
        async with send:
            await send.write(b"concurrent data")

    async def do_read():
        return await recv.read()

    write_task = asyncio.create_task(do_write())
    read_task = asyncio.create_task(do_read())

    await write_task
    result = await read_task
    assert result == b"concurrent data"
    await server_task


@pytest.mark.asyncio
async def test_read_n_ignores_limit(session_pair):
    """read(5, limit=2) reads up to 5 bytes — limit is only used for read-to-EOF."""
    server_session, client_session = session_pair

    async def server_side():
        send, _recv = await server_session.accept_bi()
        async with send:
            await send.write(b"hello world")

    _send, recv = await client_session.open_bi()
    task = asyncio.create_task(server_side())

    # limit is silently ignored when n > 0
    data = await recv.read(5, limit=2)
    assert 0 < len(data) <= 5
    assert b"hello world".startswith(data)
    await task


@pytest.mark.asyncio
async def test_write_some_correctness(session_pair):
    """write_some() return value matches bytes actually received by peer."""
    server_session, client_session = session_pair

    async def server_side():
        _send_s, recv_s = await server_session.accept_bi()
        return await recv_s.read()

    server_task = asyncio.create_task(server_side())

    send, _recv = await client_session.open_bi()
    payload = b"hello world test data"
    n = await send.write_some(payload)
    assert 0 < n <= len(payload)
    await send.finish()

    received = await asyncio.wait_for(server_task, timeout=5.0)
    assert received == payload[:n]


@pytest.mark.asyncio
async def test_readexactly_zero_after_eof(session_pair):
    """readexactly(0) returns b"" even after EOF."""
    server_session, client_session = session_pair

    async def server_side():
        send, _recv = await server_session.accept_bi()
        await send.finish()

    _send, recv = await client_session.open_bi()
    task = asyncio.create_task(server_side())

    # Read until EOF
    data = await recv.read()
    assert data == b""

    # readexactly(0) should return b"" even after EOF
    data2 = await recv.readexactly(0)
    assert data2 == b""
    await task


@pytest.mark.asyncio
async def test_parallel_open_bi_streams(session_pair):
    """Open 5 bidi streams in parallel, write distinct data, all echo correctly."""
    server_session, client_session = session_pair
    n = 5

    async def server_side():
        for _ in range(n):
            send, recv = await server_session.accept_bi()
            data = await recv.read()
            async with send:
                await send.write(data)

    server_task = asyncio.create_task(server_side())

    async def client_stream(i):
        send, recv = await client_session.open_bi()
        async with send:
            await send.write(f"stream-{i}".encode())
        return await recv.read()

    results = await asyncio.gather(*[client_stream(i) for i in range(n)])
    assert sorted(results) == sorted(f"stream-{i}".encode() for i in range(n))
    await server_task


@pytest.mark.asyncio
async def test_concurrent_bidi_and_uni_streams(session_pair):
    """3 bidi + 3 uni streams simultaneously, all operate correctly."""
    server_session, client_session = session_pair
    uni_received = []

    async def handle_bidi():
        for _ in range(3):
            send, recv = await server_session.accept_bi()
            data = await recv.read()
            async with send:
                await send.write(data)

    async def handle_uni():
        for _ in range(3):
            recv = await server_session.accept_uni()
            data = await recv.read()
            uni_received.append(data)

    server_task = asyncio.gather(
        asyncio.create_task(handle_bidi()),
        asyncio.create_task(handle_uni()),
    )

    # Client: open 3 bidi + 3 uni concurrently
    async def bidi_client(i):
        send, recv = await client_session.open_bi()
        async with send:
            await send.write(f"bidi-{i}".encode())
        return await recv.read()

    async def uni_client(i):
        send = await client_session.open_uni()
        async with send:
            await send.write(f"uni-{i}".encode())

    bidi_results = await asyncio.gather(*[bidi_client(i) for i in range(3)])
    await asyncio.gather(*[uni_client(i) for i in range(3)])

    await asyncio.wait_for(server_task, timeout=5.0)

    assert sorted(bidi_results) == sorted(f"bidi-{i}".encode() for i in range(3))
    assert sorted(uni_received) == sorted(f"uni-{i}".encode() for i in range(3))


@pytest.mark.asyncio
async def test_streams_and_datagrams_simultaneously(session_pair):
    """Bidi stream echo + datagram echo running concurrently."""
    server_session, client_session = session_pair

    async def echo_stream():
        send, recv = await server_session.accept_bi()
        data = await recv.read()
        async with send:
            await send.write(data)

    async def echo_datagram():
        dgram = await server_session.receive_datagram()
        server_session.send_datagram(dgram)

    server_task = asyncio.gather(
        asyncio.create_task(echo_stream()),
        asyncio.create_task(echo_datagram()),
    )

    # Client: stream and datagram concurrently
    async def do_stream():
        send, recv = await client_session.open_bi()
        async with send:
            await send.write(b"stream-data")
        return await recv.read()

    async def do_datagram():
        client_session.send_datagram(b"dg-data")
        return await client_session.receive_datagram()

    stream_result, dg_result = await asyncio.gather(
        asyncio.create_task(do_stream()),
        asyncio.create_task(do_datagram()),
    )

    assert stream_result == b"stream-data"
    assert dg_result == b"dg-data"
    await asyncio.wait_for(server_task, timeout=5.0)


@pytest.mark.asyncio
async def test_rapid_open_close_streams(session_pair):
    """Open and immediately close 10 streams in sequence, no errors."""
    server_session, client_session = session_pair

    async def server_side():
        for _ in range(10):
            send, recv = await server_session.accept_bi()
            async with send:
                data = await recv.read()
                assert data == b""  # Client sent finish() immediately, no data
        server_session.close()

    server_task = asyncio.create_task(server_side())

    for _ in range(10):
        send, _recv = await client_session.open_bi()
        await send.finish()
    await client_session.wait_closed()

    await asyncio.wait_for(server_task, timeout=5.0)


@pytest.mark.asyncio
async def test_interleaved_stream_and_datagram(session_pair):
    """Alternating: datagram, bidi, datagram, bidi."""
    server_session, client_session = session_pair

    async def server_side():
        # datagram 1
        dg1 = await server_session.receive_datagram()
        server_session.send_datagram(dg1)
        # bidi 1
        send1, recv1 = await server_session.accept_bi()
        data1 = await recv1.read()
        async with send1:
            await send1.write(data1)
        # datagram 2
        dg2 = await server_session.receive_datagram()
        server_session.send_datagram(dg2)
        # bidi 2
        send2, recv2 = await server_session.accept_bi()
        data2 = await recv2.read()
        async with send2:
            await send2.write(data2)

    server_task = asyncio.create_task(server_side())

    results = []

    # datagram 1
    client_session.send_datagram(b"dg1")
    dg = await client_session.receive_datagram()
    results.append(("dg", dg))

    # bidi 1
    send, recv = await client_session.open_bi()
    async with send:
        await send.write(b"bi1")
    data = await recv.read()
    results.append(("bi", data))

    # datagram 2
    client_session.send_datagram(b"dg2")
    dg = await client_session.receive_datagram()
    results.append(("dg", dg))

    # bidi 2
    send, recv = await client_session.open_bi()
    async with send:
        await send.write(b"bi2")
    data = await recv.read()
    results.append(("bi", data))

    assert results == [
        ("dg", b"dg1"),
        ("bi", b"bi1"),
        ("dg", b"dg2"),
        ("bi", b"bi2"),
    ]
    await server_task


@pytest.mark.asyncio
async def test_read_with_limit_not_exceeded(session_pair):
    """read(limit=100) on 50-byte stream succeeds."""
    server_session, client_session = session_pair

    async def server_side():
        send, _recv = await server_session.accept_bi()
        async with send:
            await send.write(b"x" * 50)

    _send, recv = await client_session.open_bi()
    task = asyncio.create_task(server_side())

    data = await recv.read(limit=100)
    assert data == b"x" * 50
    await task


@pytest.mark.asyncio
async def test_multiple_uni_streams(session_pair):
    """5 uni streams from client, all received correctly by server."""
    server_session, client_session = session_pair
    n = 5
    received = []

    async def server_side():
        for _ in range(n):
            recv = await server_session.accept_uni()
            data = await recv.read()
            received.append(data)

    server_task = asyncio.create_task(server_side())

    for i in range(n):
        send = await client_session.open_uni()
        async with send:
            await send.write(f"uni-{i}".encode())

    await asyncio.wait_for(server_task, timeout=5.0)
    assert sorted(received) == sorted(f"uni-{i}".encode() for i in range(n))


@pytest.mark.asyncio
async def test_open_bi_write_finish_without_accept(session_pair):
    """open_bi + write + finish succeed without peer calling accept_bi.

    After finish, the peer can still accept and read the data.
    """
    server_session, client_session = session_pair

    send, _recv = await client_session.open_bi()
    await send.write(b"hello")
    await send.finish()

    # Now accept and verify the data was buffered
    _send_s, recv_s = await server_session.accept_bi()
    data = await recv_s.read()
    assert data == b"hello"


@pytest.mark.asyncio
async def test_open_uni_write_finish_without_accept(session_pair):
    """open_uni + write + finish succeed without peer calling accept_uni.

    After finish, the peer can still accept and read the data.
    """
    server_session, client_session = session_pair

    send = await client_session.open_uni()
    await send.write(b"hello")
    await send.finish()

    # Now accept and verify the data was buffered
    recv_s = await server_session.accept_uni()
    data = await recv_s.read()
    assert data == b"hello"
