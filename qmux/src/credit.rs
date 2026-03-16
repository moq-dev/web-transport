use tokio::sync::watch;

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
/// Clone is cheap (watch::Sender is internally Arc'd).
#[derive(Clone, Debug)]
pub struct Credit {
    inner: watch::Sender<CreditState>,
}

impl Credit {
    /// Create with initial max (used starts at 0).
    pub fn new(max: u64) -> Self {
        Self {
            inner: watch::Sender::new(CreditState {
                used: 0,
                max,
                released: 0,
                closed: false,
            }),
        }
    }

    // --- Send-side methods ---

    /// Try to claim up to `limit` units. Returns amount claimed (0 if none available).
    pub fn try_claim(&self, limit: u64) -> u64 {
        let mut claimed = 0;
        self.inner.send_if_modified(|state| {
            let available = state.max.saturating_sub(state.used);
            claimed = limit.min(available);
            if claimed > 0 {
                state.used += claimed;
                true
            } else {
                false
            }
        });
        claimed
    }

    /// Claim up to `limit` units, waiting until credit is available.
    pub async fn claim(&self, limit: u64) -> Result<u64, Error> {
        loop {
            if self.inner.borrow().closed {
                return Err(Error::Closed);
            }

            let claimed = self.try_claim(limit);
            if claimed > 0 {
                return Ok(claimed);
            }

            let mut rx = self.inner.subscribe();
            rx.wait_for(|state| state.used < state.max || state.closed)
                .await
                .map_err(|_| Error::Closed)?;
        }
    }

    /// Claim exactly 1 unit and return the index (value of `used` before incrementing).
    pub async fn claim_index(&self) -> Result<u64, Error> {
        loop {
            if self.inner.borrow().closed {
                return Err(Error::Closed);
            }

            let mut index = 0;
            let claimed = self.inner.send_if_modified(|state| {
                if state.used < state.max {
                    index = state.used;
                    state.used += 1;
                    true
                } else {
                    false
                }
            });
            if claimed {
                return Ok(index);
            }

            let mut rx = self.inner.subscribe();
            rx.wait_for(|state| state.used < state.max || state.closed)
                .await
                .map_err(|_| Error::Closed)?;
        }
    }

    /// Close the credit, causing all pending and future `claim()`/`claim_index()` calls
    /// to return `Err(Error::Closed)`.
    pub fn close(&self) {
        self.inner.send_if_modified(|state| {
            if state.closed {
                false
            } else {
                state.closed = true;
                true
            }
        });
    }

    /// Return previously claimed credit (rollback on failed send).
    pub fn release(&self, amount: u64) {
        self.inner.send_if_modified(|state| {
            let new = state.used.saturating_sub(amount);
            if new != state.used {
                state.used = new;
                true
            } else {
                false
            }
        });
    }

    /// Increase the max. Returns error if new_max < current max.
    pub fn increase_max(&self, new_max: u64) -> Result<(), Error> {
        let mut ok = true;
        self.inner.send_if_modified(|state| {
            if new_max < state.max {
                ok = false;
                return false;
            }
            if new_max == state.max {
                return false;
            }
            state.max = new_max;
            true
        });
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
        let mut ok = true;
        self.inner.send_if_modified(|state| {
            if value > state.max {
                ok = false;
                false
            } else if value > state.used {
                state.used = value;
                true
            } else {
                false
            }
        });
        ok
    }

    /// Validate and account for incoming data. Returns false if flow control is violated.
    pub fn receive(&self, len: u64) -> bool {
        let mut ok = true;
        self.inner.send_if_modified(|state| {
            if state.used + len > state.max {
                ok = false;
                false
            } else {
                state.used += len;
                true
            }
        });
        ok
    }

    /// Report that `len` bytes have been consumed by the application.
    /// Returns `Some(new_max)` if a window update should be sent.
    pub fn consume(&self, len: u64) -> Option<u64> {
        let mut update = None;
        self.inner.send_if_modified(|state| {
            state.released += len;

            // Send a window update when: used + 2*released > max
            // i.e. more than half the remaining window has been consumed.
            if state.used + 2 * state.released > state.max {
                let new_max = state.max + state.released;
                state.max = new_max;
                state.released = 0;
                update = Some(new_max);
                true
            } else {
                false
            }
        });
        update
    }
}
