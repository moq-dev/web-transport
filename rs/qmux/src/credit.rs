use std::{
    future::poll_fn,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

use crate::Error;

#[derive(Debug, Clone, Copy)]
struct CreditState {
    used: u64,
    max: u64,
    /// Bytes freed/consumed since the last window update (recv-side only).
    released: u64,
    /// Set to true when the credit is closed (session teardown).
    closed: bool,
}

/// Tracks used/max credit for flow control.
///
/// Works for both send and recv flow control:
/// - **Send**: `try_claim`/`claim` to reserve credit, `release` for rollback, `increase_max` on peer's MAX_DATA.
/// - **Recv**: `receive` to validate incoming data, `consume` to track app consumption and trigger window updates.
///
/// Clone is cheap (internally `Arc`'d).
///
/// Waiters park a [`Waker`] here rather than on a channel: a channel's wait future
/// owns its registration and drops it with the future, so a caller that polls once
/// and comes back later would never be woken. Parking the waker in the shared state
/// is what lets `poll_claim`/`poll_claim_index` be honest `poll_*` methods.
#[derive(Clone, Debug)]
pub struct Credit {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug)]
struct Inner {
    state: CreditState,
    /// Parked on credit becoming available, or the credit closing.
    wakers: Vec<Waker>,
}

impl Inner {
    /// Apply `f`, waking waiters if it reports that availability changed.
    fn update<T>(&mut self, f: impl FnOnce(&mut CreditState) -> (T, bool)) -> (T, Vec<Waker>) {
        let (out, notify) = f(&mut self.state);
        let wakers = if notify {
            std::mem::take(&mut self.wakers)
        } else {
            Vec::new()
        };
        (out, wakers)
    }

    fn park(&mut self, cx: &Context<'_>) {
        if !self.wakers.iter().any(|w| w.will_wake(cx.waker())) {
            self.wakers.push(cx.waker().clone());
        }
    }
}

/// Wake outside the lock, so a woken task never immediately blocks on it.
fn wake_all(wakers: Vec<Waker>) {
    for waker in wakers {
        waker.wake();
    }
}

impl Credit {
    /// Create with initial max (used starts at 0).
    pub fn new(max: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                state: CreditState {
                    used: 0,
                    max,
                    released: 0,
                    closed: false,
                },
                wakers: Vec::new(),
            })),
        }
    }

    // --- Send-side methods ---

    /// Try to claim up to `limit` units. Returns amount claimed (0 if none available).
    pub fn try_claim(&self, limit: u64) -> u64 {
        let mut inner = self.inner.lock().unwrap();
        let available = inner.state.max.saturating_sub(inner.state.used);
        let claimed = limit.min(available);
        inner.state.used += claimed;
        claimed
    }

    /// Claim up to `limit` units, waiting until credit is available.
    pub async fn claim(&self, limit: u64) -> Result<u64, Error> {
        poll_fn(|cx| self.poll_claim(cx, limit)).await
    }

    /// Poll to claim up to `limit` units.
    pub fn poll_claim(&self, cx: &mut Context<'_>, limit: u64) -> Poll<Result<u64, Error>> {
        let mut inner = self.inner.lock().unwrap();

        if inner.state.closed {
            return Poll::Ready(Err(Error::Closed));
        }

        let available = inner.state.max.saturating_sub(inner.state.used);
        let claimed = limit.min(available);
        if claimed > 0 {
            inner.state.used += claimed;
            return Poll::Ready(Ok(claimed));
        }

        inner.park(cx);
        Poll::Pending
    }

    /// Claim exactly 1 unit and return the index (value of `used` before incrementing).
    pub async fn claim_index(&self) -> Result<u64, Error> {
        poll_fn(|cx| self.poll_claim_index(cx)).await
    }

    /// Poll to claim exactly 1 unit, returning the index it claimed.
    pub fn poll_claim_index(&self, cx: &mut Context<'_>) -> Poll<Result<u64, Error>> {
        let mut inner = self.inner.lock().unwrap();

        if inner.state.closed {
            return Poll::Ready(Err(Error::Closed));
        }

        if inner.state.used < inner.state.max {
            let index = inner.state.used;
            inner.state.used += 1;
            return Poll::Ready(Ok(index));
        }

        inner.park(cx);
        Poll::Pending
    }

    /// Close the credit, causing all pending and future `claim()`/`claim_index()` calls
    /// to return `Err(Error::Closed)`.
    pub fn close(&self) {
        let (_, wakers) = self.inner.lock().unwrap().update(|state| {
            let changed = !state.closed;
            state.closed = true;
            ((), changed)
        });
        wake_all(wakers);
    }

    /// Return previously claimed credit (rollback on failed send).
    pub fn release(&self, amount: u64) {
        let (_, wakers) = self.inner.lock().unwrap().update(|state| {
            let new = state.used.saturating_sub(amount);
            let changed = new != state.used;
            state.used = new;
            ((), changed)
        });
        wake_all(wakers);
    }

    /// Increase the max. Returns error if new_max < current max.
    pub fn increase_max(&self, new_max: u64) -> Result<(), Error> {
        let (ok, wakers) = self.inner.lock().unwrap().update(|state| {
            if new_max < state.max {
                return (false, false);
            }
            let changed = new_max != state.max;
            state.max = new_max;
            (true, changed)
        });
        wake_all(wakers);

        if ok {
            Ok(())
        } else {
            Err(Error::FlowControlError)
        }
    }

    // --- Recv-side methods ---

    /// Set used to max(used, value). Returns false if value > max (flow control violation).
    /// Used for stream count tracking where opening index N implies all indices 0..N.
    pub fn receive_up_to(&self, value: u64) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if value > inner.state.max {
            return false;
        }
        inner.state.used = inner.state.used.max(value);
        true
    }

    /// Validate and account for incoming data. Returns false if flow control is violated.
    pub fn receive(&self, len: u64) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.state.used + len > inner.state.max {
            return false;
        }
        inner.state.used += len;
        true
    }

    /// Report that `len` bytes have been consumed by the application.
    /// Returns `Some(new_max)` if a window update should be sent.
    pub fn consume(&self, len: u64) -> Option<u64> {
        let (update, wakers) = self.inner.lock().unwrap().update(|state| {
            state.released += len;

            // Send a window update when: used + 2*released > max
            // i.e. more than half the remaining window has been consumed.
            if state.used + 2 * state.released > state.max {
                let new_max = state.max + state.released;
                state.max = new_max;
                state.released = 0;
                (Some(new_max), true)
            } else {
                (None, false)
            }
        });
        wake_all(wakers);
        update
    }
}
