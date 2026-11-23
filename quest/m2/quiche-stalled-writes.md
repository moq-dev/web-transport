# [M] Quiche writes sometimes stall until the session drops

## Goal

`higher_priority_finishes_first` in `rs/web-transport-quiche/tests/priority.rs` passes every run. It hangs to its 30 s timeout about one run in 25–60 on a loaded host (load ≈ 18), against 0.1 s normally, seen on 2026-11-22 at `main` 6308c8b with no quiche changes.

## Plan

In a hanging run with `RUST_LOG=web_transport_quiche=trace`, the server received FIN on 30 of the 32 streams. Nothing more happened until the timeout dropped the session, which then sent RESET_STREAM for client streams 10 and 34. So two client `write_all` calls stopped making progress with data left to send. That points to a lost wakeup in the send path (`ez/send.rs`, `ez/driver.rs`), like the recv-side waker fixed in [#567](https://github.com/moq-dev/web-transport/pull/567), rather than to flow control, since the server was reading every stream to EOF.

Reproduce by looping the test binary (`cargo test -p web-transport-quiche --all-features --test priority --no-run`, then run it under `timeout 40` until it fails), find the missing wake, fix it at the driver, and add a deterministic regression test next to `tests/ez_wakers.rs`.
