//! Who is waiting on a shared accept, and how they are woken.
//!
//! [`SessionAccept`](crate::SessionAccept) is shared by every clone of a session, so it
//! has to fan a single arrival out to every accepter parked on it — without holding on
//! to accepters that walk away, which is what the weak registrations in
//! [`kio::WaiterList`] are for.
//!
//! [`AcceptWaiters`] is the list, plus the waker the shared accept futures are polled
//! with. [`Parked`] is the caller's end of a registration in that list, for a caller
//! that only has a `Context` to work with.

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
