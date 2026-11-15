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
(the 0.5.4 release): drain until `TryRecvError::Disconnected`, which tokio
returns for a closed receiver only once no send is in flight, and
`std::thread::yield_now()` on `Empty`. Port its `data_racing_drop_is_credited`
test, which needs a `#[cfg(test)]` `Credit::window` accessor (adapt it to
main's `Credit`). On 0.5 that test failed about one run in three without the
fix at 100k iterations (about 1.2 s) and passed every run with it.

Caused by: [moq-dev/web-transport#520](https://github.com/moq-dev/web-transport/pull/520)
added `discard` with the `Empty`-terminated drain.

## Related

- [@moq/qmux 0.3.5](/quest/a0/qmux-js-release.md) - the JS side runs on one
  thread, so it has no such race
