use std::sync::Arc;

use tokio::sync::watch;

use crate::Error;

#[derive(Debug, Clone, Copy)]
struct CreditState {
    used: u64,
    max: u64,
    closed: bool,
}

/// Tracks used/max credit for flow control.
///
/// Clone is cheap (Arc internally). Multiple senders can claim/release
/// credit concurrently; a single receiver updates the max.
#[derive(Clone, Debug)]
pub struct Credit {
    inner: Arc<watch::Sender<CreditState>>,
}

impl Credit {
    /// Create with initial max (used starts at 0).
    pub fn new(max: u64) -> Self {
        Self {
            inner: Arc::new(watch::Sender::new(CreditState {
                used: 0,
                max,
                closed: false,
            })),
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
    /// Returns amount claimed (always > 0 unless closed).
    pub async fn claim(&self, limit: u64) -> Result<u64, Error> {
        loop {
            let claimed = self.try_claim(limit);
            if claimed > 0 {
                return Ok(claimed);
            }

            // Check if closed before waiting
            if self.inner.borrow().closed {
                return Err(Error::Closed);
            }

            // Wait until state changes (max increases, used decreases, or closed)
            let mut rx = self.inner.subscribe();
            rx.wait_for(|state| state.closed || state.used < state.max)
                .await
                .map_err(|_| Error::Closed)?;

            if self.inner.borrow().closed {
                return Err(Error::Closed);
            }
        }
    }

    /// Return previously claimed credit (for rollback).
    pub fn release(&self, amount: u64) {
        self.inner.send_if_modified(|state| {
            state.used = state.used.saturating_sub(amount);
            true
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

    /// Close the credit, causing pending and future `claim()` calls to return `Err`.
    #[allow(dead_code)]
    pub fn close(&self) {
        self.inner.send_if_modified(|state| {
            if state.closed {
                return false;
            }
            state.closed = true;
            true
        });
    }

    /// Get current available credit (max - used).
    #[allow(dead_code)]
    pub fn available(&self) -> u64 {
        let state = *self.inner.borrow();
        state.max.saturating_sub(state.used)
    }
}
