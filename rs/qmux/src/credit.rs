use std::task::Poll;

use kio::Waiter;

use crate::Error;

#[derive(Debug, Clone, Copy, Default)]
struct CreditState {
    used: u64,
    max: u64,
    /// Bytes freed/consumed since the last window update (recv-side only).
    released: u64,
    /// Set to true when the credit is closed (session teardown).
    closed: bool,
}

impl CreditState {
    fn available(&self) -> u64 {
        self.max.saturating_sub(self.used)
    }
}

/// Tracks used/max credit for flow control.
///
/// Works for both send and recv flow control:
/// - **Send**: `poll_claim`/`poll_claim_index` to reserve credit, `release` for rollback, `increase_max` on peer's MAX_DATA.
/// - **Recv**: `receive` to validate incoming data, `consume` to track app consumption and trigger window updates.
///
/// Clone is cheap and shares the same state.
///
/// Backed by [`kio::Shared`], so waiters park in the shared state rather than
/// inside a wait future: a caller that polls once and comes back later is still
/// woken. `poll_claim`/`poll_claim_index` take a [`Waiter`], letting one caller
/// wait on this credit and other kio channels in a single poll. Any mutation
/// wakes every parked claimant (kio has no wake-one); losers re-check and
/// re-park.
#[derive(Clone, Debug)]
pub struct Credit {
    state: kio::Shared<CreditState>,
}

impl Credit {
    /// Create with initial max (used starts at 0).
    pub fn new(max: u64) -> Self {
        Self {
            state: kio::Shared::new(CreditState {
                used: 0,
                max,
                released: 0,
                closed: false,
            }),
        }
    }

    // --- Send-side methods ---

    /// Try to claim up to `limit` units. Returns amount claimed (0 if none available).
    #[cfg(test)]
    pub fn try_claim(&self, limit: u64) -> u64 {
        let mut state = self.state.lock();
        let claimed = limit.min(state.available());
        if claimed > 0 {
            state.used += claimed;
        }
        claimed
    }

    /// Poll to claim up to `limit` units.
    pub fn poll_claim(&self, waiter: &Waiter, limit: u64) -> Poll<Result<u64, Error>> {
        self.state
            .poll(waiter, |state| {
                if state.closed || state.available() > 0 {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .map(|mut state| {
                if state.closed {
                    return Err(Error::Closed);
                }
                let claimed = limit.min(state.available());
                state.used += claimed;
                Ok(claimed)
            })
    }

    /// Claim exactly 1 unit and return the index (value of `used` before incrementing).
    pub async fn claim_index(&self) -> Result<u64, Error> {
        kio::wait(|waiter| self.poll_claim_index(waiter)).await
    }

    /// Poll to claim exactly 1 unit, returning the index it claimed.
    pub fn poll_claim_index(&self, waiter: &Waiter) -> Poll<Result<u64, Error>> {
        self.state
            .poll(waiter, |state| {
                if state.closed || state.available() > 0 {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .map(|mut state| {
                if state.closed {
                    return Err(Error::Closed);
                }
                let index = state.used;
                state.used += 1;
                Ok(index)
            })
    }

    /// Close the credit, causing all pending and future `claim()`/`claim_index()` calls
    /// to return `Err(Error::Closed)`.
    pub fn close(&self) {
        let mut state = self.state.lock();
        if !state.closed {
            state.closed = true;
        }
    }

    /// Return previously claimed credit (rollback on failed send).
    pub fn release(&self, amount: u64) {
        let mut state = self.state.lock();
        let new = state.used.saturating_sub(amount);
        if new != state.used {
            state.used = new;
        }
    }

    /// Increase the max. Returns error if new_max < current max.
    pub fn increase_max(&self, new_max: u64) -> Result<(), Error> {
        let mut state = self.state.lock();
        if new_max < state.max {
            return Err(Error::FlowControlError);
        }
        if new_max != state.max {
            state.max = new_max;
        }
        Ok(())
    }

    // --- Recv-side methods ---

    /// Set used to max(used, value). Returns false if value > max (flow control violation).
    /// Used for stream count tracking where opening index N implies all indices 0..N.
    pub fn receive_up_to(&self, value: u64) -> bool {
        let mut state = self.state.lock();
        if value > state.max {
            return false;
        }
        if value > state.used {
            state.used = value;
        }
        true
    }

    /// Validate and account for incoming data. Returns false if flow control is violated.
    pub fn receive(&self, len: u64) -> bool {
        let mut state = self.state.lock();
        if state.used + len > state.max {
            return false;
        }
        state.used += len;
        true
    }

    /// Report that `len` bytes have been consumed by the application.
    /// Returns `Some(new_max)` if a window update should be sent.
    pub fn consume(&self, len: u64) -> Option<u64> {
        let mut state = self.state.lock();
        state.released += len;

        // Send a window update when: used + 2*released > max
        // i.e. more than half the remaining window has been consumed.
        if state.used + 2 * state.released > state.max {
            let new_max = state.max + state.released;
            state.max = new_max;
            state.released = 0;
            Some(new_max)
        } else {
            None
        }
    }
}
