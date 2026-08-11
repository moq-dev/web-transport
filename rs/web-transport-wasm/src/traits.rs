//! The [`web_transport_trait`] implementations: both the async surface and the
//! sans-I/O poll surface.
//!
//! Every type in this crate holds a `JsValue`, which is never `Send`, and the traits
//! require `MaybeSend` — vacuous on `wasm32` and `Send` everywhere else. So these
//! impls only exist on `wasm32`, which is the only place the crate does anything: a
//! host build compiles to keep `cargo check --workspace` honest and nothing more.

use std::{
    future::poll_fn,
    task::{Context, Poll},
};

use bytes::Bytes;
use web_transport_trait::{Stats, StatsUnavailable};

use crate::{error::stream_error_code, Error, RecvStream, SendStream, Session};

impl web_transport_trait::Error for Error {
    /// The browser reports one numeric code either way, so the variant is the only
    /// thing saying which registry it belongs to. Answering both would have a stream
    /// reset decoded against the session table, since callers ask about the session
    /// first.
    fn session_error(&self) -> Option<(u32, String)> {
        match self {
            Error::Session(err) => Some((stream_error_code(err).unwrap_or(0), err.message())),
            _ => None,
        }
    }

    fn stream_error(&self) -> Option<u32> {
        match self {
            Error::Stream(err) => stream_error_code(err),
            _ => None,
        }
    }
}

impl web_transport_trait::poll::Session for Session {
    type SendStream = SendStream;
    type RecvStream = RecvStream;
    type Error = Error;

    fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<RecvStream, Error>> {
        Session::poll_accept_uni(self, cx)
    }

    fn poll_accept_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(SendStream, RecvStream), Error>> {
        Session::poll_accept_bi(self, cx)
    }

    fn poll_open_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<SendStream, Error>> {
        Session::poll_open_uni(self, cx)
    }

    fn poll_open_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(SendStream, RecvStream), Error>> {
        Session::poll_open_bi(self, cx)
    }

    fn poll_send_datagram(
        &mut self,
        cx: &mut Context<'_>,
        payload: &[u8],
    ) -> Poll<Result<(), Error>> {
        Session::poll_send_datagram(self, cx, payload)
    }

    fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Error>> {
        Session::poll_recv_datagram(self, cx)
    }

    fn max_datagram_size(&self) -> usize {
        Session::max_datagram_size(self)
    }

    fn protocol(&self) -> Option<&str> {
        Session::protocol(self)
    }

    fn close(&mut self, code: u32, reason: &str) {
        Session::close(self, code, reason)
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Error> {
        Session::poll_closed(self, cx)
    }

    /// The browser only reports statistics through a promise, which this cannot wait
    /// on.
    fn stats(&self) -> impl Stats {
        StatsUnavailable
    }
}

impl web_transport_trait::Session for Session {
    type SendStream = SendStream;
    type RecvStream = RecvStream;
    type Error = Error;

    async fn accept_uni(&self) -> Result<RecvStream, Error> {
        Session::accept_uni(self).await
    }

    async fn accept_bi(&self) -> Result<(SendStream, RecvStream), Error> {
        Session::accept_bi(self).await
    }

    async fn open_uni(&self) -> Result<SendStream, Error> {
        Session::open_uni(self).await
    }

    async fn open_bi(&self) -> Result<(SendStream, RecvStream), Error> {
        Session::open_bi(self).await
    }

    /// This trait's send is synchronous while the browser's is a promise, so the
    /// payload is handed over without waiting for capacity. A datagram was never
    /// guaranteed to arrive anyway.
    fn send_datagram(&self, payload: Bytes) -> Result<(), Error> {
        // A datagram the browser has no room for is dropped rather than queued;
        // the trait already lists that among the ways one fails to arrive.
        Session::try_send_datagram(self, &payload).map(|_| ())
    }

    async fn recv_datagram(&self) -> Result<Bytes, Error> {
        Session::recv_datagram(self).await
    }

    fn max_datagram_size(&self) -> usize {
        Session::max_datagram_size(self)
    }

    fn protocol(&self) -> Option<&str> {
        Session::protocol(self)
    }

    fn close(&self, code: u32, reason: &str) {
        Session::close(self, code, reason)
    }

    async fn closed(&self) -> Error {
        Session::closed(self).await
    }

    fn stats(&self) -> impl Stats {
        StatsUnavailable
    }
}

impl web_transport_trait::poll::SendStream for SendStream {
    type Error = Error;

    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Error>> {
        SendStream::poll_write(self, cx, buf)
    }

    fn set_priority(&mut self, order: u8) {
        SendStream::set_priority(self, order.into())
    }

    fn finish(&mut self) -> Result<(), Error> {
        SendStream::finish(self)
    }

    fn reset(&mut self, code: u32) {
        SendStream::reset(self, code)
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.poll_closed_raw(cx)
    }
}

impl web_transport_trait::SendStream for SendStream {
    type Error = Error;

    async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        poll_fn(|cx| SendStream::poll_write(self, cx, buf)).await
    }

    fn set_priority(&mut self, order: u8) {
        SendStream::set_priority(self, order.into())
    }

    fn finish(&mut self) -> Result<(), Error> {
        SendStream::finish(self)
    }

    fn reset(&mut self, code: u32) {
        SendStream::reset(self, code)
    }

    async fn closed(&mut self) -> Result<(), Error> {
        poll_fn(|cx| self.poll_closed_raw(cx)).await
    }
}

impl web_transport_trait::poll::RecvStream for RecvStream {
    type Error = Error;

    fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, Error>> {
        RecvStream::poll_read(self, cx, dst)
    }

    /// Overridden because the browser already handed us the bytes: slicing the
    /// buffered chunk avoids the default's allocate-and-copy.
    fn poll_read_chunk(
        &mut self,
        cx: &mut Context<'_>,
        max: usize,
    ) -> Poll<Result<Option<Bytes>, Error>> {
        RecvStream::poll_read_chunk(self, cx, max)
    }

    fn stop(&mut self, code: u32) {
        RecvStream::stop(self, code)
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.poll_closed_raw(cx)
    }
}

impl web_transport_trait::RecvStream for RecvStream {
    type Error = Error;

    async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, Error> {
        poll_fn(|cx| RecvStream::poll_read(self, cx, dst)).await
    }

    async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, Error> {
        RecvStream::read(self, max).await
    }

    fn stop(&mut self, code: u32) {
        RecvStream::stop(self, code)
    }

    async fn closed(&mut self) -> Result<(), Error> {
        poll_fn(|cx| self.poll_closed_raw(cx)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The traits are the point of this module: a signature that drifts out of
    /// conformance should fail here rather than in a dependent crate.
    #[test]
    fn implements_both_surfaces() {
        fn session<S: web_transport_trait::Session + web_transport_trait::poll::Session>() {}
        fn send<S: web_transport_trait::SendStream + web_transport_trait::poll::SendStream>() {}
        fn recv<S: web_transport_trait::RecvStream + web_transport_trait::poll::RecvStream>() {}

        session::<Session>();
        send::<SendStream>();
        recv::<RecvStream>();
    }
}
