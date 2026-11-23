# [S] Surface quiche server socket errors

## Goal

When a `web_transport_quiche` server's socket task fails, `Server::accept` reports the error instead of returning `None` as if the server shut down cleanly.

## Plan

`ez::Server::new` spawns one `run_socket` task per socket into a `JoinSet` that nothing ever joins, so an `io::Error` from a socket is dropped (`rs/web-transport-quiche/src/ez/server.rs`, which also carries a `TODO close all when one errors`). With one socket, `accept` then returns `None`, which callers treat as a normal shutdown. Reproduce it with a socket that errors, then have `accept` return the error and close the remaining sockets when one fails. `accept` returning `Option` today makes this a breaking change to `ez::Server::accept` and `Server::accept`.
