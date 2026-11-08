# [S] Run QMux's TLS and WSS paths on rind

## Goal

qmux's `tls` and `wss` features compile no aws-lc-rs or ring. A connection the caller doesn't configure runs on rind-rustls, and one the caller configures keeps their `rustls` config.

## Plan

Today aws-lc-rs comes in through qmux's own `rustls` and `tokio-rustls` dependencies (both with default features) and through `tokio-tungstenite/rustls-tls-native-roots`, which enables rustls's default provider. Depend on rustls and tokio-rustls with `default-features = false`, and pick tokio-tungstenite features that bring no provider. Where qmux or tungstenite would build a default `ClientConfig` (a WSS connect with no connector), build it on the installed process provider, else rind, with native roots. A CPU rind refuses returns an error.

Check every qmux feature combination with `cargo tree -e normal,build -i aws-lc-sys` and `-i ring`. `tls::Client`/`tls::Server` always take a caller config, so they get no default. Test a TLS handshake with a caller config built on rind, and a WSS handshake with no caller config. Whether to also change the `qmux-v0.5.x` line is a separate decision, not part of this quest.
