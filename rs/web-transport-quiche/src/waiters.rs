//! Who is waiting on shared connection state, and how they are woken.
//!
//! Both this crate and [`ez`](crate::ez) park callers on state they share — the accept
//! queues, the connection-closed error, a stream's flow-control credit — and none of it
//! may hold on to a caller that walks away. That is what the weak registrations in
//! [`kio::WaiterList`] are for: a slot lives only as long as the [`kio::Waiter`] that
//! made it.
//!
//! [`Parked`] is the caller's end of such a registration, a [`kio::Park`] that also lets
//! go of its waiter once the poll finishes. [`AcceptWaiters`] is the other end for
//! [`SessionAccept`](crate::SessionAccept), which is shared by every clone of a session
//! and has to fan one arrival out to every accepter parked on it.

use std::{
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake},
};

use kio::{Park, Waiter, WaiterList};

#[derive(Default)]
struct AcceptState {
    waiters: WaiterList,

    /// How many polls are holding the lock on the accept state. While this is non-zero
    /// a wake is recorded rather than delivered. See [`AcceptWaiters::arm`].
    ///
    /// A count, not a flag: one poll releases the accept lock before it disarms, so
    /// another can be armed and mid-poll by then, and a flag would let the first one
    /// clear the second's protection.
    armed: usize,

    /// A wake arrived while `armed`, and is owed to the list.
    deferred: bool,
}

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
    state: Mutex<AcceptState>,
}

impl AcceptWaiters {
    /// Park a caller until the next wake.
    ///
    /// Registrations are weak and owned by the caller, so an accepter that gives up — a
    /// `timeout` around `accept_uni`, or a task that just drops the future — releases
    /// its slot instead of leaving a waker parked here forever.
    pub(crate) fn register(&self, waiter: &Waiter) {
        waiter.register(&mut self.state.lock().unwrap().waiters);
    }

    /// Wake every parked accepter so it can retry, or record the wake while a poll is
    /// [armed](Self::arm).
    ///
    /// The list is taken under the lock and woken outside it. A waker is free to resume
    /// its task inline, and the first thing a resumed accepter does is register here
    /// again — waking underneath the lock would deadlock on this very mutex.
    pub(crate) fn wake_all(&self) {
        let mut waiters = {
            let mut state = self.state.lock().unwrap();
            if state.armed > 0 {
                state.deferred = true;
                return;
            }

            state.waiters.take()
        };

        waiters.wake();
    }

    /// Hold back wakes for the duration of a poll that holds the lock on the accept
    /// state.
    ///
    /// That poll drives the shared accept futures with this list's waker, and one of
    /// them may wake it *inline* — `FuturesUnordered`, for one, notifies its parent from
    /// a child's `wake`. Delivering that would resume an accepter underneath the accept
    /// lock, which it takes as its first move. So it is recorded instead, and
    /// [`disarm`](Self::disarm) hands it back once the lock is gone.
    pub(crate) fn arm(&self) {
        self.state.lock().unwrap().armed += 1;
    }

    /// Stop holding wakes back, reporting whether one arrived and is this caller's to
    /// deliver.
    ///
    /// Only ever called with the accept lock released, and the caller owes the list a
    /// [`wake_all`](Self::wake_all) when this returns true. False while another poll is
    /// still armed: a deferred wake stays owed until the last one leaves, and that one
    /// delivers it.
    pub(crate) fn disarm(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        state.armed = state.armed.saturating_sub(1);
        if state.armed > 0 {
            return false;
        }

        std::mem::take(&mut state.deferred)
    }

    /// How many slots the list is holding, read out of its `Debug` — [`WaiterList`]
    /// exposes no length, and the test below has to see that repeated polls do not stack
    /// entries. Panics rather than guessing if kio's format ever changes.
    #[cfg(test)]
    fn slots(&self) -> usize {
        let list = format!("{:?}", self.state.lock().unwrap().waiters);
        list.rsplit_once("len: ")
            .and_then(|(_, len)| len.trim_end_matches(&[' ', '}'][..]).parse().ok())
            .expect("WaiterList Debug should report a length")
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

/// A [`kio::Park`] that lets go of its waiter once the poll finishes.
///
/// `Park` retains the waiter until the next poll or until it drops, which is what a
/// pending operation needs: a registration in [`AcceptWaiters`] (or any
/// [`kio::WaiterList`]) lives only as long as the [`Waiter`] that made it. A *finished*
/// poll is the other case. The stream is done with that caller, and going on holding its
/// waker pins the polling task's allocation until something polls the cell again — for a
/// stream whose last read completed and then sat idle, that is the rest of the
/// connection. So this releases it on `Ready`.
///
/// `Park` also clones empty, so a cloned handle never inherits another handle's live
/// registration.
///
/// The `async` methods have no need for any of it — [`kio::wait`] keeps the waiter inside
/// the future it builds, so dropping the future drops the registration.
#[derive(Clone, Default)]
pub(crate) struct Parked(Park);

impl Parked {
    /// Run one poll with a retained waiter, releasing it if the poll finishes.
    pub(crate) fn poll<T>(
        &mut self,
        cx: &Context<'_>,
        poll: impl FnOnce(&Waiter) -> Poll<T>,
    ) -> Poll<T> {
        let waiter = self.hold(cx);
        let result = poll(&waiter);
        self.settle(&result);
        result
    }

    /// Hold a waiter for this poll, for a body that needs `&mut self` of the struct
    /// holding this cell — the borrow checker allows only one of those at a time, so the
    /// closure form above will not compile there. Pair it with [`settle`](Self::settle).
    ///
    /// The returned clone shares the parked waiter's identity, so nothing is lost by
    /// taking one, and it ends the borrow on the cell.
    pub(crate) fn hold(&mut self, cx: &Context<'_>) -> Waiter {
        self.0.hold(cx).clone()
    }

    /// Release the held waiter if the poll finished.
    pub(crate) fn settle<T>(&mut self, result: &Poll<T>) {
        if result.is_ready() {
            self.0 = Park::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{mpsc, Weak},
        thread,
        time::Duration,
    };

    use super::*;

    /// Records whether it was woken.
    #[derive(Default)]
    struct Flag {
        woken: std::sync::atomic::AtomicBool,
    }

    impl Flag {
        fn woken(&self) -> bool {
            self.woken.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.woken.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn cloning_a_pending_handle_does_not_retain_the_poller() {
        let waiters = AcceptWaiters::default();
        let flag = Arc::new(Flag::default());
        let waker = std::task::Waker::from(flag.clone());
        let mut parked = Parked::default();

        assert!(parked
            .poll(&Context::from_waker(&waker), |waiter| {
                waiters.register(waiter);
                Poll::<()>::Pending
            })
            .is_pending());

        let cloned = parked.clone();
        drop(parked);
        waiters.wake_all();

        assert!(
            !flag.woken(),
            "the clone retained the original handle's parked poller"
        );
        drop(cloned);
    }

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

    /// Repeated polls must not stack registrations.
    ///
    /// Both routes in build a fresh `Waiter` per poll and drop the previous one — kio's
    /// `wait` for the `async` methods, the `Context` bridge for the `poll_*` ones — so
    /// the list reclaims the dead slot instead of growing. A bridge that reused a *live*
    /// waiter would add an entry on every re-poll, which is the trap `kio::WaiterCell`
    /// warns about, and this is the guard for whatever ends up replacing that bridge.
    #[test]
    fn repeated_polls_do_not_stack_registrations() {
        let waiters = AcceptWaiters::default();

        // Retained across iterations exactly as a poll bridge retains it: assigning the
        // next one drops the previous, killing the registration it left behind.
        let mut held = None;

        for _ in 0..100 {
            let waiter = Waiter::new(std::task::Waker::noop().clone());
            waiters.register(&waiter);
            held = Some(waiter);
        }

        assert!(
            waiters.slots() <= 2,
            "100 polls left {} registrations behind",
            waiters.slots()
        );

        drop(held);
    }

    /// A wake that lands mid-poll is held until the accept lock is gone.
    ///
    /// The shared accept futures are driven with this list's waker while the poll holds
    /// that lock, and an inner future can wake it inline. Delivering it there would
    /// resume an accepter straight into the lock it is standing on.
    #[test]
    fn a_wake_during_a_poll_is_deferred() {
        let waiters = AcceptWaiters::default();

        let flag = Arc::new(Flag::default());
        let waiter = Waiter::new(std::task::Waker::from(flag.clone()));
        waiters.register(&waiter);

        waiters.arm();
        waiters.wake_all();
        assert!(
            !flag.woken(),
            "the wake was delivered while the accept lock was held"
        );

        assert!(waiters.disarm(), "the deferred wake was dropped");
        waiters.wake_all();
        assert!(flag.woken(), "the deferred wake never arrived");
    }

    /// Nothing owed when nothing woke.
    #[test]
    fn disarming_a_quiet_poll_owes_no_wake() {
        let waiters = AcceptWaiters::default();
        waiters.arm();
        assert!(!waiters.disarm());
    }

    /// One poll releasing the accept lock must not drop another's protection.
    ///
    /// The lock is released before `disarm`, so a second poll can be armed and running
    /// by then. With a flag instead of a count, the first one leaving would expose the
    /// second to exactly the inline wake this is here to prevent.
    #[test]
    fn overlapping_polls_stay_armed_until_the_last_one_leaves() {
        let waiters = AcceptWaiters::default();

        let flag = Arc::new(Flag::default());
        let waiter = Waiter::new(std::task::Waker::from(flag.clone()));
        waiters.register(&waiter);

        waiters.arm(); // first poll
        waiters.arm(); // second poll, overlapping

        assert!(!waiters.disarm(), "the first out does not own the wake");

        waiters.wake_all();
        assert!(!flag.woken(), "a poll is still holding the accept lock");

        assert!(waiters.disarm(), "the last one out owes the deferred wake");
        waiters.wake_all();
        assert!(flag.woken());
    }
}
