# [S] A finished quiche read sometimes still holds its waker

## Goal

`a_finished_read_releases_the_poller` in
`rs/web-transport-quiche/tests/ez_wakers.rs` passes every run. It fails about
one run in 200 on `main` (0cb7412), and failed CI's `build` job on
[moq-dev/web-transport#545](https://github.com/moq-dev/web-transport/pull/545),
which changes only a workflow: after the read completes and the test drops its
own `Waker`, `Arc::strong_count` of the poller's waker is 2, not 1.

## Plan

Find whether the extra reference is a real retention in the ez read path or
the test racing the driver: the test runs on a two-thread runtime, so a wake
in flight on the driver thread may hold a clone just as the count is read.
Fix the retention if it is one; if it is a transient clone, make the test
wait on the event that releases it rather than reading the count once,
without a sleep or retry. Reproduce with
`for i in $(seq 200); do cargo test -q -p web-transport-quiche --test ez_wakers a_finished_read_releases_the_poller || break; done`.

Caused by: [moq-dev/web-transport#353](https://github.com/moq-dev/web-transport/pull/353)
added the test and the waker release it checks.
