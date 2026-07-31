//! Who is waiting on a shared accept, and how they are woken.
//!
//! `H3SessionAccept` is shared by every clone of a session, so it has to fan a single
//! arrival out to every accepter parked on it — without holding on to accepters that
//! walk away, which is what the weak registrations in [`kio::WaiterList`] are for.
//!
//! [`AcceptWaiters`] is that list, plus the waker the shared accept futures are polled
//! with. A caller holds the other end of its registration inside the future it is
//! polling; see [`kio::wait`].

use std::{
    sync::{Arc, Mutex},
    task::Wake,
};

use kio::{Waiter, WaiterList};

/// The accepters parked on one direction, plus the waker that wakes all of them.
///
/// The shared accept futures are polled with a [`Waker`](std::task::Waker) made from
/// this, not with the caller's. A caller's waker goes stale the moment it stops
/// accepting: whoever polled last is the one the inner future holds, so if that caller
/// drops its future the arrival wakes nobody while other accepters sit parked. This
/// waker cannot go stale — it lives as long as the accept state — and waking it wakes
/// everyone on the list.
#[derive(Default)]
pub(crate) struct AcceptWaiters {
    // Held only to register or to wake, never across a poll of the inner futures, which
    // take locks of their own further down.
    waiters: Mutex<WaiterList>,
}

impl AcceptWaiters {
    /// Park a caller until the next wake.
    ///
    /// Registrations are weak and owned by the caller, so an accepter that gives up — a
    /// `timeout` around `accept_uni`, or a task that just drops the future — releases
    /// its slot instead of leaving a waker parked here forever.
    pub(crate) fn register(&self, waiter: &Waiter) {
        waiter.register(&mut self.waiters.lock().unwrap());
    }

    /// Wake every parked accepter so it can retry.
    ///
    /// The list is taken under the lock and woken outside it. A waker is free to resume
    /// its task inline, and the first thing a resumed accepter does is register here
    /// again — waking underneath the lock would deadlock on this very mutex.
    pub(crate) fn wake_all(&self) {
        let mut waiters = self.waiters.lock().unwrap().take();
        waiters.wake();
    }
}

impl Wake for AcceptWaiters {
    fn wake(self: Arc<Self>) {
        self.wake_all();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wake_all();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Weak, mpsc},
        thread,
        time::Duration,
    };

    use super::*;

    /// A waker that re-enters the list from `wake`, standing in for an executor that
    /// polls a resumed task inline: the first thing a resumed accepter does is register
    /// itself again.
    struct Reentrant {
        waiters: Weak<AcceptWaiters>,
    }

    impl Wake for Reentrant {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            if let Some(waiters) = self.waiters.upgrade() {
                waiters.register(&Waiter::new(std::task::Waker::noop().clone()));
            }
        }
    }

    #[test]
    fn waking_does_not_hold_the_lock() {
        let waiters = Arc::new(AcceptWaiters::default());
        let inline = Arc::new(Reentrant {
            waiters: Arc::downgrade(&waiters),
        });

        // Kept alive for the whole test: the registration dies with it.
        let waiter = Waiter::new(std::task::Waker::from(inline));
        waiters.register(&waiter);

        // On a deadlock this thread never finishes, so the test cannot hang.
        let (tx, rx) = mpsc::channel();
        thread::spawn({
            let waiters = waiters.clone();
            move || {
                waiters.wake_all();
                let _ = tx.send(());
            }
        });

        rx.recv_timeout(Duration::from_secs(5))
            .expect("wake_all woke a waker while holding the lock, and the wake re-entered it");
    }
}
