# web-transport

[![PyPI](https://img.shields.io/pypi/v/web-transport-quinn)](https://pypi.org/project/web-transport-quinn/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Python bindings for [web-transport-quinn](https://docs.rs/web-transport-quinn/) — a pure-Rust WebTransport library built on QUIC.

## Features

- **Server and client** — one session per connection, no HTTP/3 pooling complexity
- **Streams** — bidirectional and unidirectional, with backpressure
- **Datagrams** — unreliable, low-latency messaging
- **Certificate pinning** — connect to self-signed servers by hash
- **Async-native** — built for `asyncio`, works with `async with` and `async for`
- **Fast** — Rust core compiled to a native extension via PyO3

## Installation

```bash
pip install web-transport-quinn
```

Requires Python 3.12+. Prebuilt wheels are available for Linux, macOS, and Windows.

## Quick start: Echo server

```python
import asyncio
import web_transport

async def main():
    cert, key = web_transport.generate_self_signed(["localhost"])

    async with web_transport.Server(
        certificate_chain=[cert],
        private_key=key,
        bind="[::]:4433",
    ) as server:
        print(f"Listening on {server.local_addr}")
        print(f"Certificate hash: {web_transport.certificate_hash(cert).hex()}")

        async for request in server:
            session = await request.accept()
            asyncio.create_task(handle(session))

async def handle(session: web_transport.Session):
    async with session:
        send, recv = await session.accept_bi()
        async with send:
            data = await recv.read()
            await send.write(data)

asyncio.run(main())
```

## Quick start: Client

```python
import asyncio
import web_transport

async def main():
    async with web_transport.Client(no_cert_verification=True) as client:
        session = await client.connect("https://localhost:4433")

        async with session:
            send, recv = await session.open_bi()
            async with send:
                await send.write(b"Hello, WebTransport!")
            response = await recv.read()
            print(response)

asyncio.run(main())
```

## API overview

| Name | Description |
|---|---|
| `Server` | Listens for incoming WebTransport sessions |
| `SessionRequest` | Inspect and accept/reject incoming sessions |
| `Client` | Opens outgoing WebTransport sessions |
| `Session` | Established session — open streams, send datagrams |
| `SendStream` | Write to a QUIC stream |
| `RecvStream` | Read from a QUIC stream |
| `generate_self_signed()` | Create a self-signed certificate and private key (DER bytes) |
| `certificate_hash()` | SHA-256 fingerprint of a DER-encoded certificate |

`Server` and `Client` accept DER-encoded `bytes` for certificates and keys, so you can use any TLS library (e.g. [`cryptography`](https://cryptography.io/)) to generate or load them.

All exceptions inherit from `web_transport.WebTransportError`. See the [type stubs](python/web_transport/_web_transport.pyi) for the full API reference.

## Certificate pinning

Connect to servers with self-signed certificates by pinning their SHA-256 hash:

```python
cert_hash = web_transport.certificate_hash(cert)  # or bytes.fromhex("ab12cd34...")

async with web_transport.Client(
    server_certificate_hashes=[cert_hash],
) as client:
    session = await client.connect("https://localhost:4433")
```

## Datagrams

Send and receive unreliable datagrams over an established session:

```python
await session.send_datagram(b"ping")
data = await session.receive_datagram()
```

The maximum payload size is available via `session.max_datagram_size`.

## Development

```bash
# From py/web-transport/ in the monorepo:
uv sync
uv run maturin develop
uv run pytest
```

## License

MIT OR Apache-2.0
