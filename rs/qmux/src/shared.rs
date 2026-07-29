//! A receiver shared by every clone of a session.

use std::{
    future::poll_fn,
    sync::Mutex,
    task::{Context, Poll, Waker},
};

use tokio::sync::mpsc;

/// An `mpsc::Receiver` that any number of handles can poll.
///
/// Two things make this different from wrapping the receiver in a
/// `tokio::sync::Mutex`:
///
/// - **The lock is held only for the poll itself.** A `tokio::sync::Mutex` invites
///   `lock().await` followed by `recv().await`, which holds the guard across the
///   wait — so a handle that starts an accept and stops polling blocks every other
///   clone until it is polled again or dropped. That is a hang, not a slowdown, and
///   it is unrepresentable here: a `std::sync::MutexGuard` is not `Send`, so the
///   compiler rejects holding one across an await.
///
/// - **Wakers are tracked per caller.** `mpsc::Receiver::poll_recv` stores exactly
///   one waker, so a second caller would replace the first's registration and the
///   first would never wake. Every waiter is parked here instead, and they are all
///   woken whenever one of them takes a value, so the losers re-poll and
///   re-register.
#[derive(Debug)]
pub(crate) struct SharedRecv<T> {
    inner: Mutex<Inner<T>>,
}

#[derive(Debug)]
struct Inner<T> {
    rx: mpsc::Receiver<T>,
    wakers: Vec<Waker>,
}

impl<T> SharedRecv<T> {
    pub fn new(rx: mpsc::Receiver<T>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                rx,
                wakers: Vec::new(),
            }),
        }
    }

    /// Poll for the next value, `None` once the channel is closed and drained.
    pub fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let mut inner = self.inner.lock().unwrap();

        match inner.rx.poll_recv(cx) {
            Poll::Ready(value) => {
                // Whoever else is parked lost the race for this value, but the
                // receiver now holds only *our* waker. Wake them so they re-poll and
                // re-register, otherwise they wait forever on a registration that no
                // longer exists.
                let wakers = std::mem::take(&mut inner.wakers);
                drop(inner);

                for waker in wakers {
                    waker.wake();
                }

                Poll::Ready(value)
            }
            Poll::Pending => {
                if !inner.wakers.iter().any(|w| w.will_wake(cx.waker())) {
                    inner.wakers.push(cx.waker().clone());
                }
                Poll::Pending
            }
        }
    }

    /// Wait for the next value, `None` once the channel is closed and drained.
    pub async fn recv(&self) -> Option<T> {
        poll_fn(|cx| self.poll_recv(cx)).await
    }
}
