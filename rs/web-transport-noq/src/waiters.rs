//! Who is waiting on a shared accept, and how they are woken.
//!
//! `SessionAccept` is shared by every clone of a session, so it has to fan a single
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
    pub(crate) fn wake_all(&self) {
        self.waiters.lock().unwrap().wake();
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
