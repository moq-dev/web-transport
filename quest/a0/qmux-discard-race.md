# [S] Credit receive data racing a discarded stream on main

## Goal

On `main`, `RecvStream::discard` in `rs/qmux/src/session.rs` returns the
connection credit of every byte the reader delivers concurrently with a stop or
drop. Today it drains with `while let Ok(..) = try_recv()`, which stops at
`TryRecvError::Empty`. A reader send that counted its message before `close()`
but pushes it after that `Empty` still succeeds, so neither side credits those
bytes. On a multi-threaded runtime, MAX_DATA leaks a little at a time until the
connection stalls.

## Plan

Port the fix from
[moq-dev/web-transport#543](https://github.com/moq-dev/web-transport/pull/543)
(the 0.5.4 release): a per-stream `Unread` counter (an `AtomicU64` with a
stopped bit) shared by the backend `RecvState` and the `RecvStream`. The reader
counts a chunk's bytes before queueing it, or credits them itself once the
frontend has stopped. The frontend uncounts a chunk when it takes it, and
`discard` sets the stopped bit and credits whatever is still counted.

Don't drain until `TryRecvError::Disconnected` instead: tokio returns that for
a closed receiver with a live sender only since 1.49, and qmux allows
`tokio >= 1.27`, so on older tokio that loop spins forever (reproduced on #543).

Port #543's `data_racing_drop_is_credited` test, which needs a `#[cfg(test)]`
`Credit::window` accessor (adapt it to main's `Credit`). On 0.5, without the
fix, that test failed about one run in three at 100k iterations (about 1.2 s);
with it, it passed every run.

Caused by: [moq-dev/web-transport#520](https://github.com/moq-dev/web-transport/pull/520)
added `discard` with the `Empty`-terminated drain.
