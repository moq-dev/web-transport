use tokio::sync::watch;

use crate::Error;

#[derive(Debug, Clone, Copy)]
struct CreditState {
    used: u64,
    max: u64,
}

/// Tracks used/max credit for flow control.
///
/// Clone is cheap (watch::Sender is internally Arc'd).
/// Multiple holders can claim/release credit concurrently via `send_if_modified`.
#[derive(Clone, Debug)]
pub struct Credit {
    inner: watch::Sender<CreditState>,
}

impl Credit {
    /// Create with initial max (used starts at 0).
    pub fn new(max: u64) -> Self {
        Self {
            inner: watch::Sender::new(CreditState { used: 0, max }),
        }
    }

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
    /// Returns amount claimed (always > 0 unless the sender is dropped).
    pub async fn claim(&self, limit: u64) -> Result<u64, Error> {
        loop {
            let claimed = self.try_claim(limit);
            if claimed > 0 {
                return Ok(claimed);
            }

            // Wait until state changes (max increases or used decreases)
            let mut rx = self.inner.subscribe();
            rx.wait_for(|state| state.used < state.max)
                .await
                .map_err(|_| Error::Closed)?;
        }
    }

    /// Return previously claimed credit (for rollback).
    pub fn release(&self, amount: u64) {
        self.inner.send_if_modified(|state| {
            state.used = state.used.saturating_sub(amount);
            true
        });
    }

    /// Claim exactly 1 unit and return the index (value of `used` before incrementing).
    /// Waits until credit is available.
    pub async fn claim_index(&self) -> Result<u64, Error> {
        loop {
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
            rx.wait_for(|state| state.used < state.max)
                .await
                .map_err(|_| Error::Closed)?;
        }
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

}
