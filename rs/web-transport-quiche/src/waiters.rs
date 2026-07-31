//! Who is waiting on shared connection state, and how they are woken.
//!
//! Both this crate and [`ez`](crate::ez) park callers on state they share — the accept
//! queues, the connection-closed error, a stream's flow-control credit — and none of it
//! may hold on to a caller that walks away. That is what the weak registrations in
//! [`kio::WaiterList`] are for: a slot lives only as long as the [`kio::Waiter`] that
//! made it.
//!
//! [`Parked`] is the caller's end of such a registration, for a caller that has only a
//! `Context` to work with. [`AcceptWaiters`] is the other end for
//! [`SessionAccept`](crate::SessionAccept), which is shared by every clone of a session
//! and has to fan one arrival out to every accepter parked on it.

use std::{
    sync::{Arc, Mutex},
    task::{Context, Wake},
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

/// A [`Waiter`] retained across `poll` calls.
///
/// A registration in [`AcceptWaiters`] stays live only while the caller holds the
/// [`Waiter`] it registered, which is what lets a caller that walks away release its
/// slot. So the handle has to live somewhere the *caller* owns, and a `poll_*` method is
/// handed nothing but a `Context`. This is that somewhere: one cell per operation, held
/// by whoever polls.
///
/// The `async` methods have no need for it — [`kio::wait`] keeps the waiter inside the
/// future it builds, so dropping the future drops the registration.
///
/// kio grew a `WaiterCell` for exactly this after 0.5.2 (moq-dev/moq#2560); replace this
/// with it once that releases. Its `hold` also *reuses* the waiter when the task is
/// unchanged and every registration was already drained, which saves the allocation this
/// one makes on each poll.
#[derive(Default)]
pub(crate) struct Parked {
    waiter: Option<Waiter>,
}

impl Parked {
    /// Adopt `cx`'s waker for this poll, retiring the previous handle and with it the
    /// registration it left behind.
    ///
    /// Always replaces rather than reusing: [`WaiterList`] reclaims a slot only when the
    /// `Waiter` that registered it dies, so re-registering a live one would stack a
    /// duplicate entry on every spurious re-poll.
    pub(crate) fn hold(&mut self, cx: &mut Context<'_>) -> &Waiter {
        self.waiter.insert(Waiter::new(cx.waker().clone()))
    }
}

impl Parked {
    /// Park a waiter built for this poll, retiring the previous one.
    ///
    /// The two-step alternative to [`hold`](Self::hold), for a poll that needs `&mut
    /// self` of the struct holding this cell while the waiter is alive — the borrow
    /// checker allows only one of those at a time. Build the waiter from the `Context`,
    /// poll with it, then park it here so its registrations outlive the poll.
    pub(crate) fn park(&mut self, waiter: Waiter) {
        // Retiring the old one *after* the poll matters: the new waiter is already
        // registered by then, so there is no window with nothing registered.
        self.waiter = Some(waiter);
    }
}

impl Clone for Parked {
    /// A clone starts unregistered: a registration belongs to the handle that parked it.
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for Parked {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Parked").finish_non_exhaustive()
    }
}
