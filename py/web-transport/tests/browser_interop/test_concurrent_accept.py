"""Browser interop tests for concurrent accept from multiple tasks.

Regression tests for the lost-waker bug: multiple asyncio tasks calling
accept_bi() or accept_uni() concurrently on the same session must all
complete without hanging.
"""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING

import pytest

if TYPE_CHECKING:
    from .conftest import RunJS, ServerFactory

pytestmark = pytest.mark.asyncio(loop_scope="session")


async def test_concurrent_accept_bi_from_multiple_tasks(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """3 independent tasks call accept_bi() concurrently; all must complete."""
    n = 3
    async with start_server() as (server, port, hash_b64):
        completed: int = 0

        async def server_side() -> None:
            nonlocal completed
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:

                async def accept_and_echo(task_id: int) -> None:
                    nonlocal completed
                    send, recv = await session.accept_bi()
                    async with send:
                        data = await recv.read()
                        await send.write(data)
                    completed += 1

                async with asyncio.TaskGroup() as inner_tg:
                    for i in range(n):
                        inner_tg.create_task(
                            asyncio.wait_for(accept_and_echo(i), timeout=5.0)
                        )

                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                f"""
                const N = {n};
                const promises = [];
                for (let i = 0; i < N; i++) {{
                    promises.push((async () => {{
                        const stream = await transport.createBidirectionalStream();
                        await writeAllString(stream.writable, "bi-" + i);
                        return await readAllString(stream.readable);
                    }})());
                }}
                const results = await Promise.all(promises);
                return results.sort();
            """,
            )

    expected = sorted(f"bi-{i}" for i in range(n))
    assert result == expected
    assert completed == n


async def test_concurrent_accept_bi_and_uni_from_multiple_tasks(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """3 accept_bi + 3 accept_uni tasks concurrently; all 6 must complete."""
    n = 3
    async with start_server() as (server, port, hash_b64):
        completed: int = 0

        async def server_side() -> None:
            nonlocal completed
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:

                async def accept_bi_echo(task_id: int) -> None:
                    nonlocal completed
                    send, recv = await session.accept_bi()
                    async with send:
                        data = await recv.read()
                        await send.write(data)
                    completed += 1

                async def accept_uni_read(task_id: int) -> None:
                    nonlocal completed
                    recv = await session.accept_uni()
                    await recv.read()
                    completed += 1

                async with asyncio.TaskGroup() as inner_tg:
                    for i in range(n):
                        inner_tg.create_task(
                            asyncio.wait_for(accept_bi_echo(i), timeout=5.0)
                        )
                        inner_tg.create_task(
                            asyncio.wait_for(accept_uni_read(i), timeout=5.0)
                        )

                # Signal browser that all streams were processed
                signal = await session.open_uni()
                async with signal:
                    await signal.write(b"ok")
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            result = await run_js(
                port,
                hash_b64,
                f"""
                const N = {n};
                const promises = [];

                for (let i = 0; i < N; i++) {{
                    promises.push((async () => {{
                        const stream = await transport.createBidirectionalStream();
                        await writeAllString(stream.writable, "bi-" + i);
                        return await readAllString(stream.readable);
                    }})());
                }}
                for (let i = 0; i < N; i++) {{
                    promises.push((async () => {{
                        const stream = await transport.createUnidirectionalStream();
                        await writeAllString(stream, "uni-" + i);
                        return "uni-sent-" + i;
                    }})());
                }}
                const results = await Promise.all(promises);

                // Wait for server signal
                const reader = transport.incomingUnidirectionalStreams.getReader();
                const {{ value: signalStream }} = await reader.read();
                const sr = signalStream.getReader();
                await sr.read();
                sr.releaseLock();
                reader.releaseLock();

                const biResults = results.slice(0, N).sort();
                return biResults;
            """,
            )

    expected = sorted(f"bi-{i}" for i in range(n))
    assert result == expected
    assert completed == n * 2


async def test_concurrent_accept_uni_from_multiple_tasks(
    start_server: ServerFactory, run_js: RunJS
) -> None:
    """3 independent tasks call accept_uni() concurrently; all must complete."""
    n = 3
    async with start_server() as (server, port, hash_b64):
        received: list[bytes] = []

        async def server_side() -> None:
            request = await server.accept()
            assert request is not None
            session = await request.accept()
            async with session:

                async def accept_uni_read(task_id: int) -> None:
                    recv = await session.accept_uni()
                    data = await recv.read()
                    received.append(data)

                async with asyncio.TaskGroup() as inner_tg:
                    for i in range(n):
                        inner_tg.create_task(
                            asyncio.wait_for(accept_uni_read(i), timeout=5.0)
                        )

                # Signal browser
                signal = await session.open_uni()
                async with signal:
                    await signal.write(b"ok")
                await session.wait_closed()

        async with asyncio.TaskGroup() as tg:
            tg.create_task(server_side())
            await run_js(
                port,
                hash_b64,
                f"""
                const N = {n};
                const promises = [];
                for (let i = 0; i < N; i++) {{
                    promises.push((async () => {{
                        const stream = await transport.createUnidirectionalStream();
                        await writeAllString(stream, "uni-" + i);
                    }})());
                }}
                await Promise.all(promises);

                // Wait for server signal
                const reader = transport.incomingUnidirectionalStreams.getReader();
                const {{ value: signalStream }} = await reader.read();
                const sr = signalStream.getReader();
                await sr.read();
                sr.releaseLock();
                reader.releaseLock();
                return true;
            """,
            )

    assert len(received) == n
    assert sorted(received) == sorted(f"uni-{i}".encode() for i in range(n))
