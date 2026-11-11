# [S] Fix the flaky Quinn crypto handshake test

## Goal

`crypto::upstream_handshake_retry_key_update_and_reset` in `rs/web-transport-quinn/tests/crypto.rs` passes deterministically. Today it fails about 1.5% of the time because a datagram is missing in `Pair::traffic`, and that can fail the required `build` check on any PR.

## Plan

Reproduce it locally with a loop, as the #508 merge agent did (3 failures in 200 runs), and identify the mechanism before changing anything. A datagram sent in `Pair::traffic` must be received after `drive()`, but QUIC datagrams are unreliable: congestion control, pacing, or a datagram queued behind a key update or Retry can legitimately drop or delay one. Decide whether the test relies on an unguaranteed delivery or whether the rind-backed crypto adapter (`crypto/session.rs`, packet keys) really loses a packet around the key update. Fix the lowest responsible layer: an adapter bug gets a fix plus a regression test that fails without it; a test assumption gets a test that checks a guaranteed property, such as driving until delivery under controlled time. Do not add retries, sleeps or wider timeouts. Confirm with a loop of at least several hundred runs.
