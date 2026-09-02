//! Connection-wide mapping from the transport's `i32` send order onto quiche's
//! 8-bit stream urgency.
//!
//! The application hands us an `i32` where *higher* is sent first, so callers can
//! bit-pack a composite ordering into one value. quiche instead schedules by
//! `urgency`, a `u8` where *lower* is sent first, so the range has to be
//! compressed. Rather than rescale (which would collapse nearby values that the
//! caller went out of its way to distinguish), the streams are ranked: the
//! highest-priority level gets urgency 0, the next 1, and so on. Everything past
//! [`BANDS`] shares the last urgency, so the top 256 levels are ordered exactly
//! and the tail is left to round-robin among itself.
//!
//! Streams that share a send order share a level, and so share an urgency —
//! quiche then round-robins between them (`incremental`), matching what quinn and
//! qmux do with equal priorities.
//!
//! Ranks are relative, so one stream moving can shift every stream below it. The
//! updates are collected here and applied by the driver, which is the only place
//! holding a `quiche::Connection`.

use std::collections::{btree_map, BTreeMap, HashMap, HashSet};

use super::{StreamId, DEFAULT_PRIORITY};

/// How many distinct priority levels quiche can express.
const BANDS: usize = u8::MAX as usize + 1;

/// The last urgency, shared by every level past the top [`BANDS`].
const OVERFLOW: u8 = (BANDS - 1) as u8;

/// The streams sharing one send order, plus the urgency last published for them.
struct Level {
    band: u8,
    streams: HashSet<StreamId>,
}

/// Ranks every send stream on a connection, emitting the quiche urgency updates
/// that ranking implies.
#[derive(Default)]
pub(super) struct Priorities {
    /// Levels keyed by send order, so iterating in reverse walks them from the
    /// highest priority down.
    levels: BTreeMap<i32, Level>,

    /// The send order each registered stream currently sits at.
    streams: HashMap<StreamId, i32>,

    /// Urgency changes the driver has yet to hand to quiche.
    pending: HashMap<StreamId, u8>,
}

impl Priorities {
    /// Register a new send stream at the default send order.
    ///
    /// Every send stream is registered, not just the ones the application
    /// prioritizes: an unranked stream would keep quiche's own default urgency and
    /// so could outrank a stream that explicitly asked to go last.
    pub fn insert(&mut self, id: StreamId) {
        self.rank(id, DEFAULT_PRIORITY);
    }

    /// Change a stream's send order, where higher values are sent first.
    ///
    /// Ignored for a stream that isn't registered, which means the driver has
    /// already retired it. The application can still be holding the handle then —
    /// a peer `STOP_SENDING` retires a stream underneath a live `SendStream` — and
    /// ranking it again would strand a level that nothing is left to remove,
    /// pushing every live stream below it down a band, plus an urgency update that
    /// can never be applied because the stream is gone from the driver's map.
    pub fn set(&mut self, id: StreamId, order: i32) {
        if !self.streams.contains_key(&id) {
            return;
        }

        self.rank(id, order);
    }

    /// Register or move `id` to `order`.
    fn rank(&mut self, id: StreamId, order: i32) {
        if self.streams.get(&id) == Some(&order) {
            return;
        }

        // A level appearing or disappearing shifts every level below it.
        let mut shifted = match self.streams.insert(id, order) {
            Some(previous) => self.detach(id, previous),
            None => false,
        };

        match self.levels.entry(order) {
            btree_map::Entry::Occupied(mut entry) => {
                let level = entry.get_mut();
                level.streams.insert(id);

                // The level itself hasn't moved, so only this stream needs telling.
                self.pending.insert(id, level.band);
            }
            btree_map::Entry::Vacant(entry) => {
                // Seed at the tail band and publish it: `rebalance` corrects the
                // level if it lands higher, but it is also allowed to stop before
                // reaching a level that genuinely belongs down here.
                entry.insert(Level {
                    band: OVERFLOW,
                    streams: HashSet::from([id]),
                });
                self.pending.insert(id, OVERFLOW);
                shifted = true;
            }
        }

        if shifted {
            self.rebalance();
        }
    }

    /// Drop a stream that has closed, promoting whatever it was holding back.
    pub fn remove(&mut self, id: StreamId) {
        let Some(order) = self.streams.remove(&id) else {
            return;
        };

        // The stream is gone; an urgency update for it would only resurrect state.
        self.pending.remove(&id);

        if self.detach(id, order) {
            self.rebalance();
        }
    }

    /// Take the urgency updates accumulated so far, to be applied to quiche.
    pub fn take(&mut self) -> HashMap<StreamId, u8> {
        std::mem::take(&mut self.pending)
    }

    /// The send order `id` is registered at.
    #[cfg(test)]
    pub fn order(&self, id: StreamId) -> Option<i32> {
        self.streams.get(&id).copied()
    }

    /// The urgency currently assigned to `id`, if it is registered.
    #[cfg(test)]
    pub fn urgency(&self, id: StreamId) -> Option<u8> {
        let order = self.streams.get(&id)?;
        Some(self.levels.get(order)?.band)
    }

    /// Requeue updates the driver couldn't apply yet, for the next attempt.
    ///
    /// A stream registers here as soon as it is opened, but quiche only learns of
    /// it on the driver's next pass, so an urgency can be ready before there is
    /// anything to apply it to. Anything queued since the [`take`](Self::take) is
    /// newer and wins.
    pub fn defer(&mut self, updates: impl IntoIterator<Item = (StreamId, u8)>) {
        for (id, urgency) in updates {
            self.pending.entry(id).or_insert(urgency);
        }
    }

    /// Remove `id` from the level at `order`, reporting whether the level itself
    /// went away.
    fn detach(&mut self, id: StreamId, order: i32) -> bool {
        let btree_map::Entry::Occupied(mut entry) = self.levels.entry(order) else {
            return false;
        };

        entry.get_mut().streams.remove(&id);
        if entry.get().streams.is_empty() {
            entry.remove();
            return true;
        }

        false
    }

    /// Re-rank the levels, queueing an update for every stream whose urgency moved.
    fn rebalance(&mut self) {
        for (index, level) in self.levels.values_mut().rev().enumerate() {
            let band = index.min(OVERFLOW as usize) as u8;

            if level.band == band {
                // Every rebalance leaves each level's band correct and one call
                // adds or drops at most one level, so a level can only shift by a
                // single rank before the next rebalance. Reaching an unchanged
                // overflow level therefore means every level below it was already
                // at or past [`OVERFLOW`] beforehand, and is still there now.
                if band == OVERFLOW {
                    break;
                }
                continue;
            }

            level.band = band;
            for &id in &level.streams {
                self.pending.insert(id, band);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(id: u64) -> StreamId {
        StreamId::from(id)
    }

    /// Open a stream and give it a send order, the same two steps `open_uni`
    /// followed by `set_priority` takes. `set` alone is ignored for a stream the
    /// driver hasn't registered.
    fn open(p: &mut Priorities, id: StreamId, order: i32) {
        p.insert(id);
        p.set(id, order);
    }

    #[test]
    fn ranks_by_descending_send_order() {
        let mut p = Priorities::default();
        p.insert(sid(0));
        open(&mut p, sid(4), 100);
        open(&mut p, sid(8), 50);

        let pending = p.take();
        assert_eq!(pending[&sid(4)], 0);
        assert_eq!(pending[&sid(8)], 1);
        assert_eq!(pending[&sid(0)], 2);
    }

    #[test]
    fn equal_send_orders_share_a_band() {
        let mut p = Priorities::default();
        open(&mut p, sid(0), 7);
        open(&mut p, sid(4), 7);
        open(&mut p, sid(8), 3);

        let pending = p.take();
        assert_eq!(pending[&sid(0)], 0);
        assert_eq!(pending[&sid(4)], 0);
        assert_eq!(pending[&sid(8)], 1);
    }

    /// A promotion has to push everything it jumped over down a band, or the
    /// promoted stream would merely tie with them.
    #[test]
    fn promotion_shifts_the_streams_it_passed() {
        let mut p = Priorities::default();
        open(&mut p, sid(0), 30);
        open(&mut p, sid(4), 20);
        open(&mut p, sid(8), 10);
        let _ = p.take();

        p.set(sid(8), 40);

        let pending = p.take();
        assert_eq!(pending[&sid(8)], 0);
        assert_eq!(pending[&sid(0)], 1);
        assert_eq!(pending[&sid(4)], 2);
    }

    /// Closing a stream frees its band, so the streams below it move up.
    #[test]
    fn removal_promotes_the_streams_below() {
        let mut p = Priorities::default();
        open(&mut p, sid(0), 30);
        open(&mut p, sid(4), 20);
        open(&mut p, sid(8), 10);
        let _ = p.take();

        p.remove(sid(0));

        let pending = p.take();
        assert!(!pending.contains_key(&sid(0)));
        assert_eq!(pending[&sid(4)], 0);
        assert_eq!(pending[&sid(8)], 1);
    }

    /// Losing one of several streams at the same send order doesn't move anything:
    /// the level is still occupied.
    #[test]
    fn removal_within_a_level_does_not_shift() {
        let mut p = Priorities::default();
        open(&mut p, sid(0), 30);
        open(&mut p, sid(4), 30);
        open(&mut p, sid(8), 10);
        let _ = p.take();

        p.remove(sid(0));

        assert!(p.take().is_empty());
    }

    /// Only the top [`BANDS`] levels are ordered; the rest tie at the last band.
    #[test]
    fn levels_past_the_last_band_share_it() {
        let mut p = Priorities::default();
        // Descending send order, so stream N lands at rank N.
        for i in 0..(BANDS as u64 + 8) {
            open(&mut p, sid(i * 4), -(i as i32));
        }

        let pending = p.take();
        assert_eq!(pending[&sid(0)], 0);
        assert_eq!(pending[&sid((BANDS as u64 - 1) * 4)], OVERFLOW);
        assert_eq!(pending[&sid((BANDS as u64 + 7) * 4)], OVERFLOW);
    }

    /// A stream that arrives already in the overflow band still needs an update:
    /// quiche's own default urgency sits in the middle of the range, not at the end.
    #[test]
    fn overflow_streams_are_still_published() {
        let mut p = Priorities::default();
        for i in 0..BANDS as u64 {
            open(&mut p, sid(i * 4), -(i as i32));
        }
        let _ = p.take();

        let late = sid((BANDS as u64 + 1) * 4);
        open(&mut p, late, i32::MIN);

        assert_eq!(p.take()[&late], OVERFLOW);
    }

    /// A `SendStream` outlives the driver's record of it — a peer `STOP_SENDING`
    /// retires the stream while the application still holds the handle. A
    /// `set_priority` arriving then must not resurrect it: the level would never be
    /// cleaned up (nothing is left to remove it), it would displace every live
    /// stream below it, and its urgency update could never be applied.
    #[test]
    fn updates_to_a_retired_stream_are_ignored() {
        let mut p = Priorities::default();
        open(&mut p, sid(0), 30);
        open(&mut p, sid(4), 20);
        let _ = p.take();

        p.remove(sid(0));
        let _ = p.take();

        p.set(sid(0), i32::MAX);

        assert!(
            p.take().is_empty(),
            "a retired stream must not queue an update"
        );
        assert_eq!(
            p.order(sid(0)),
            None,
            "a retired stream must stay unregistered"
        );
        assert_eq!(
            p.urgency(sid(4)),
            Some(0),
            "the live stream must keep the band the retirement promoted it to"
        );
    }

    /// The same, for a stream that was never registered at all.
    #[test]
    fn updates_to_an_unknown_stream_are_ignored() {
        let mut p = Priorities::default();
        open(&mut p, sid(0), 30);
        let _ = p.take();

        p.set(sid(99), i32::MAX);

        assert!(p.take().is_empty());
        assert_eq!(p.urgency(sid(0)), Some(0));
    }

    /// The early exit in [`Priorities::rebalance`] is the one piece of the ranking
    /// that isn't obviously safe: it stops at the first unchanged overflow level,
    /// betting that everything below it is already there. Drive a long pseudorandom
    /// sequence of opens, re-rankings and closes and check every level against a
    /// from-scratch ranking after each step.
    ///
    /// The run has to spend most of its time *past* [`BANDS`] levels or the early
    /// exit is never reached and this proves nothing, so it grows the live set well
    /// beyond that and churns there. The `saw_overflow` assertion keeps it honest.
    #[test]
    fn rebalance_matches_a_full_ranking() {
        // xorshift, so the sequence is fixed without pulling in a rng crate.
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };

        // Enough distinct send orders that the live streams spread across more than
        // [`BANDS`] levels rather than piling into a few.
        let spread = 4 * BANDS as u64;
        let target = 2 * BANDS;

        let mut p = Priorities::default();
        let mut live: Vec<StreamId> = Vec::new();
        let mut saw_overflow = false;

        for step in 0..10_000u64 {
            let roll = rand() % 100;

            // Open-heavy until the live set is past `target`, then balanced, so the
            // run climbs over the overflow boundary and keeps crossing back and forth.
            let open_odds = if live.len() < target { 70 } else { 34 };

            if roll < open_odds || live.is_empty() {
                let id = sid(step * 4);
                p.insert(id);
                if rand() % 2 == 0 {
                    p.set(id, (rand() % spread) as i32 - spread as i32 / 2);
                }
                live.push(id);
            } else if roll < open_odds + 15 {
                let id = live[(rand() % live.len() as u64) as usize];
                p.set(id, (rand() % spread) as i32 - spread as i32 / 2);
            } else {
                let id = live.swap_remove((rand() % live.len() as u64) as usize);
                p.remove(id);
            }

            saw_overflow |= p.levels.len() > BANDS;

            let expected: Vec<u8> = (0..p.levels.len())
                .map(|i| i.min(OVERFLOW as usize) as u8)
                .collect();
            let actual: Vec<u8> = p.levels.values().rev().map(|l| l.band).collect();
            assert_eq!(actual, expected, "bands diverged at step {step}");

            // Nothing is left behind: every level is occupied and every stream is
            // in the level its send order names.
            for (order, level) in &p.levels {
                assert!(!level.streams.is_empty(), "empty level at step {step}");
                for id in &level.streams {
                    assert_eq!(
                        p.streams.get(id),
                        Some(order),
                        "stray stream at step {step}"
                    );
                }
            }
            assert_eq!(
                p.streams.len(),
                p.levels.values().map(|l| l.streams.len()).sum::<usize>(),
                "stream count diverged at step {step}"
            );
        }

        assert!(
            saw_overflow,
            "the run never exceeded {BANDS} levels, so the overflow early exit went untested"
        );
    }

    #[test]
    fn setting_the_same_order_twice_is_a_noop() {
        let mut p = Priorities::default();
        open(&mut p, sid(0), 5);
        let _ = p.take();

        p.set(sid(0), 5);
        assert!(p.take().is_empty());
    }
}
