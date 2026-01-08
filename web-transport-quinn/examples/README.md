# Example

## Simple Echo
A simple [server](echo-server.rs) and [client](echo-client.rs).

There's also advanced examples [server](echo-server-advanced.rs) and [client](echo-client-advanced.rs) that construct the QUIC connection manually.

QUIC requires TLS, which makes the initial setup a bit more involved.

-   Generate a certificate: `../dev/setup`
-   Run the Rust server: `cargo run --example echo-server -- --tls-cert ../dev/localhost.crt --tls-key ../dev/localhost.key`
-   Run the Rust client: `cargo run --example echo-client -- --tls-cert ../dev/localhost.crt`
-   Run a Web client: `cd ../web-demo; npm install; npx parcel serve client.html --open`

If you get a certificate error with the web client, try deleting `.parcel-cache`.

## Multiprotocol negotiation

There is another example that shows how to implement webtransport sub protocol negotiation to support several protocols, in accordance with [draft 14 of the WebTransport specification](https://www.ietf.org/archive/id/draft-ietf-webtrans-http3-14.html#section-3.3).

This example uses a self-signed certificate and an dangerously insecure client setup for easy bootstrapping. Don't use that at home!

To try the example, run the server (it uses the port `56789` by default) from the root:
```
cargo run --example multiproto-server
```

Once that is up, you can try the client with the different protocols in a separate terminal:

for the `echo` protocol run:
```
cargo run --example multiproto-client -- --protocol echo/0 "any test string"
```
You should see the server sending back the same string.

To run the `ping` protocol run:
```
cargo run --example multiproto-client -- --protocol ping/0 ping
```
You will see the server sending back a `ack` response.
