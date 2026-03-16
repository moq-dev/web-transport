# Example

A simple [server](echo-server.rs) and [client](echo-client.rs).

QUIC requires TLS, which makes the initial setup a bit more involved.
However, quiche doesn't support client certificates, so we have to disable verification anyway.

# Commands
- cd `web-transport-quiche`
-   Generate a certificate: `../dev/setup`
-   Run the Rust server: `cargo run --example echo-server -- --tls-cert ../dev/localhost.crt --tls-key ../dev/localhost.key`
-   Run the Rust client: `cargo run --example echo-client -- --tls-disable-verify`
-   Run a Web client: `cd ../web-demo; npm install; npx parcel serve client.html --open`

If you get a certificate error with the web client, try deleting `.parcel-cache`.
