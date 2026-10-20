use std::{
    future::poll_fn,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

use tokio::sync::mpsc;

use crate::{Error, Frame};

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
/// - **Recv**: `receive` to validate incoming data, `consume` to track app consumption and queue window updates.
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

    /// Snapshot the two distinct stream-count credits while both locks are held.
    pub(crate) fn available_pair(bidi: &Self, uni: &Self) -> (u64, u64) {
        let bidi = bidi.inner.lock().unwrap();
        let uni = uni.inner.lock().unwrap();
        let available = |state: &CreditState| {
            if state.closed {
                0
            } else {
                state.max.saturating_sub(state.used)
            }
        };
        (available(&bidi.state), available(&uni.state))
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

    /// Raise the max to `new_max`. A non-increasing value is ignored, as RFC 9000
    /// §19.9–§19.11 require for MAX_DATA, MAX_STREAM_DATA and MAX_STREAMS: an
    /// update can arrive after a larger one, so it is stale rather than invalid.
    pub fn increase_max(&self, new_max: u64) {
        let ((), wakers) = self.inner.lock().unwrap().update(|state| {
            let changed = new_max > state.max;
            if changed {
                state.max = new_max;
            }
            ((), changed)
        });
        wake_all(wakers);
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

    /// Report that `len` units have been consumed by the application, queueing
    /// `frame(new_max)` on `control` when a window update is due.
    ///
    /// The update is queued before the lock is released, so concurrent consumers
    /// of one credit queue their limits in increasing order. Computing it under the
    /// lock and sending afterwards would let a smaller limit overtake a larger one.
    /// The unbounded send never blocks or reenters this credit.
    pub(crate) fn consume(
        &self,
        len: u64,
        control: &mpsc::UnboundedSender<Frame>,
        frame: impl FnOnce(u64) -> Frame,
    ) {
        let mut inner = self.inner.lock().unwrap();
        let ((), wakers) = inner.update(|state| {
            state.released += len;

            // Send a window update when: used + 2*released > max
            // i.e. more than half the remaining window has been consumed.
            if state.used + 2 * state.released > state.max {
                let new_max = state.max + state.released;
                state.max = new_max;
                state.released = 0;
                control.send(frame(new_max)).ok();
                ((), true)
            } else {
                ((), false)
            }
        });
        drop(inner);
        wake_all(wakers);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc as std_mpsc, Arc,
        },
        task::{Context, Poll, Wake, Waker},
        thread,
        time::Duration,
    };

    use tokio::sync::mpsc;

    use super::Credit;
    use crate::Frame;

    #[derive(Default)]
    struct CountingWaker(AtomicUsize);

    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<Frame>) -> Vec<u64> {
        let mut out = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            match frame {
                Frame::MaxData(max) => out.push(max),
                other => panic!("unexpected frame {other:?}"),
            }
        }
        out
    }

    /// Two consumers race to replenish one credit. The first is held while it
    /// queues its update; the second must not overtake it with a larger limit.
    #[test]
    fn concurrent_updates_are_queued_in_increasing_order() {
        let credit = Credit::new(4);
        assert!(credit.receive(4));
        let (control, mut rx) = mpsc::unbounded_channel();

        let (inside_tx, inside_rx) = std_mpsc::channel();
        let (release_tx, release_rx) = std_mpsc::channel::<()>();
        let first = thread::spawn({
            let credit = credit.clone();
            let control = control.clone();
            move || {
                credit.consume(2, &control, |max| {
                    inside_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Frame::MaxData(max)
                });
            }
        });
        inside_rx.recv().unwrap();

        let (done_tx, done_rx) = std_mpsc::channel();
        let second = thread::spawn({
            let credit = credit.clone();
            let control = control.clone();
            move || {
                credit.consume(2, &control, Frame::MaxData);
                done_tx.send(()).unwrap();
            }
        });

        // The wait only gives an implementation that queues after unlocking a
        // chance to overtake; a correct one blocks here for the full timeout.
        assert!(
            done_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "the second update was queued while the first was still being queued"
        );
        release_tx.send(()).unwrap();
        first.join().unwrap();
        second.join().unwrap();

        assert_eq!(drain(&mut rx), [6, 8]);
    }

    /// A consumer that doesn't cross the replenish threshold queues nothing.
    #[test]
    fn consume_below_threshold_queues_nothing() {
        let credit = Credit::new(10);
        assert!(credit.receive(2));
        let (control, mut rx) = mpsc::unbounded_channel();
        credit.consume(2, &control, Frame::MaxData);
        assert!(drain(&mut rx).is_empty());
        credit.consume(4, &control, Frame::MaxData);
        assert_eq!(drain(&mut rx), [16]);
    }

    /// A lower or equal limit keeps the current credit and leaves waiters parked;
    /// a higher one releases them.
    #[test]
    fn non_increasing_limits_are_ignored_without_waking() {
        let credit = Credit::new(2);
        assert_eq!(credit.try_claim(2), 2);

        let counter = Arc::new(CountingWaker::default());
        let waker = Waker::from(counter.clone());
        let mut cx = Context::from_waker(&waker);
        assert!(credit.poll_claim(&mut cx, 1).is_pending());

        credit.increase_max(1);
        credit.increase_max(2);
        assert_eq!(counter.0.load(Ordering::SeqCst), 0);
        assert_eq!(credit.try_claim(1), 0);

        credit.increase_max(3);
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
        assert!(matches!(credit.poll_claim(&mut cx, 1), Poll::Ready(Ok(1))));
    }
}
