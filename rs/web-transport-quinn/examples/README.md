# Examples

## Echo

A simple [server](echo-server.rs) and [client](echo-client.rs).
The advanced [server](echo-server-advanced.rs) and [client](echo-client-advanced.rs) construct the QUIC connection manually.

QUIC requires TLS, so first generate a short-lived certificate. From the repository root:

-   Generate a certificate: `dev/setup`
-   Run the Rust server: `cargo run -p web-transport-quinn --example echo-server -- --tls-cert dev/localhost.crt --tls-key dev/localhost.key`
-   Run the Rust client: `cargo run -p web-transport-quinn --example echo-client -- --tls-cert dev/localhost.crt`
-   Run a Web client: `cd js/web-demo && bun install && bunx parcel serve client.html --open`

The web client pins the certificate's hash, so rerun `dev/setup` once the certificate expires after 10 days.
If you get a certificate error with the web client, try deleting `.parcel-cache`.
