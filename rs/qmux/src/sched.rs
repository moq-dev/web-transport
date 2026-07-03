//! Per-stream priority scheduling for outbound STREAM data.
//!
//! Replaces the bounded `mpsc` data channel the session used to interleave
//! STREAM frames purely by arrival order. Frames are bucketed by stream
//! priority (a `u8`, higher = sent first, matching the W3C `sendOrder`
//! convention) so a high-priority stream can jump ahead of a low-priority
//! backlog. Re-prioritization is retroactive and cheap: only the stream's
//! scheduling pointer moves between bands, never its queued frames, so the
//! per-stream FIFO order the receiver relies on (it appends STREAM data by
//! arrival, ignoring the wire offset) is always preserved.
//!
//! Control frames are *not* routed here — the session keeps its separate
//! unbounded `outbound_priority` channel and a `biased` select so control
//! always precedes any data this queue yields.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use tokio::sync::Notify;

use crate::{Error, Frame, StreamId};

/// Per-stream FIFO of pending frames plus the stream's current priority band.
struct StreamSlot {
    priority: u8,
    frames: VecDeque<Frame>,
}

struct Inner {
    /// Ready streams bucketed by priority. The MAX key is served first.
    /// A `StreamId` appears in at most one band at a time, and only while its
    /// slot has at least one queued frame.
    bands: BTreeMap<u8, VecDeque<StreamId>>,
    /// Per-stream queued frames + current priority.
    streams: HashMap<StreamId, StreamSlot>,
    /// Total queued frames across all streams (the capacity bound).
    len: usize,
    /// Set on session teardown; unblocks producers and the consumer.
    closed: bool,
}

impl Inner {
    /// Schedule `id` in `band` if it isn't already scheduled somewhere.
    ///
    /// Caller guarantees the slot exists and has at least one frame.
    fn arm(&mut self, id: StreamId, band: u8) {
        // A StreamId lives in at most one band. The slot's `priority` is the
        // single source of truth for which band it would be in, so checking the
        // band's queue for membership is unnecessary as long as callers only arm
        // a stream that was previously unscheduled (empty slot just created, or
        // a slot drained then refilled).
        self.bands.entry(band).or_default().push_back(id);
    }
}

/// A bounded, priority-aware queue of outbound STREAM frames shared between the
/// session's writer loop (consumer) and its `SendStream`s (producers).
///
/// Cloning shares the same underlying queue.
#[derive(Clone)]
pub struct PriorityQueue {
    inner: Arc<Mutex<Inner>>,
    /// Notified when a frame becomes available (or the queue is closed).
    non_empty: Arc<Notify>,
    /// Notified when capacity frees up (or the queue is closed).
    has_space: Arc<Notify>,
    capacity: usize,
}

impl PriorityQueue {
    /// Create a queue holding at most `capacity` frames total before `push`
    /// blocks.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                bands: BTreeMap::new(),
                streams: HashMap::new(),
                len: 0,
                closed: false,
            })),
            non_empty: Arc::new(Notify::new()),
            has_space: Arc::new(Notify::new()),
            capacity,
        }
    }

    /// Append a frame to `id`'s FIFO, blocking while the queue is full.
    ///
    /// Cancel-safe: the mutation happens synchronously right before returning,
    /// so if the caller's `select!` drops this future before it resolves, no
    /// frame is enqueued (matching the old `outbound.send` race against
    /// `inbound_stopped`).
    pub async fn push(&self, priority: u8, id: StreamId, frame: Frame) -> Result<(), Error> {
        loop {
            // Register interest *before* checking, so a `notify_one` that fires
            // between our check and `.await` isn't lost.
            let notified = self.has_space.notified();
            {
                let mut inner = self.inner.lock().unwrap();
                if inner.closed {
                    return Err(Error::Closed);
                }
                if inner.len < self.capacity {
                    self.push_locked(&mut inner, priority, id, frame);
                    return Ok(());
                }
            }
            notified.await;
        }
    }

    /// Enqueue a frame synchronously, bypassing the capacity bound — for small,
    /// must-not-be-dropped control markers like a stream FIN, which would
    /// otherwise have to either block (impossible from a sync caller) or be
    /// detached to a task (racing reset/teardown). The frame still lands in the
    /// stream's band, after its data. Fails only if the queue is closed.
    pub fn push_now(&self, priority: u8, id: StreamId, frame: Frame) -> Result<(), Error> {
        let mut inner = self.inner.lock().unwrap();
        if inner.closed {
            return Err(Error::Closed);
        }
        self.push_locked(&mut inner, priority, id, frame);
        Ok(())
    }

    fn push_locked(&self, inner: &mut Inner, priority: u8, id: StreamId, frame: Frame) {
        match inner.streams.get_mut(&id) {
            Some(slot) => {
                // Already scheduled (its band points at `id`); just append.
                slot.frames.push_back(frame);
            }
            None => {
                let mut frames = VecDeque::new();
                frames.push_back(frame);
                inner.streams.insert(id, StreamSlot { priority, frames });
                inner.arm(id, priority);
            }
        }
        inner.len += 1;
        self.non_empty.notify_one();
    }

    /// Pop the next frame to send, honoring priority then round-robin fairness
    /// among equal-priority streams. Blocks until a frame is available or the
    /// queue is closed (returns `None` once closed and drained).
    pub async fn pop(&self) -> Option<Frame> {
        loop {
            let notified = self.non_empty.notified();
            {
                let mut inner = self.inner.lock().unwrap();
                if let Some(frame) = self.pop_locked(&mut inner) {
                    return Some(frame);
                }
                if inner.closed {
                    return None;
                }
            }
            notified.await;
        }
    }

    fn pop_locked(&self, inner: &mut Inner) -> Option<Frame> {
        // Highest band first.
        let (&band, queue) = inner.bands.iter_mut().next_back()?;
        let id = queue.pop_front().expect("scheduled band must be non-empty");
        if queue.is_empty() {
            inner.bands.remove(&band);
        }

        let slot = inner
            .streams
            .get_mut(&id)
            .expect("scheduled stream must have a slot");
        let frame = slot
            .frames
            .pop_front()
            .expect("scheduled slot must be non-empty");

        if slot.frames.is_empty() {
            // Drained: drop the slot so a future push re-arms it cleanly.
            inner.streams.remove(&id);
        } else {
            // Round-robin: re-arm at the back of its *current* band (which may
            // differ from `band` if set_priority moved it; but since it was at
            // the head of the max band, its priority equals `band`).
            let priority = slot.priority;
            inner.arm(id, priority);
        }

        inner.len -= 1;
        self.has_space.notify_one();
        Some(frame)
    }

    /// Retroactively re-prioritize a stream. Frames don't move — only the
    /// scheduling pointer relocates from the old band to `new`. No-op if the
    /// stream has no queued frames.
    pub fn set_priority(&self, id: StreamId, new: u8) {
        let mut inner = self.inner.lock().unwrap();
        let old = match inner.streams.get(&id) {
            Some(slot) => slot.priority,
            None => return,
        };
        if old == new {
            return;
        }

        // Relocate the scheduling pointer if the stream is currently scheduled.
        // It is scheduled iff it has frames (invariant), which it does here.
        if let Some(queue) = inner.bands.get_mut(&old) {
            if let Some(pos) = queue.iter().position(|&s| s == id) {
                queue.remove(pos);
                if queue.is_empty() {
                    inner.bands.remove(&old);
                }
                inner.bands.entry(new).or_default().push_back(id);
            }
        }

        if let Some(slot) = inner.streams.get_mut(&id) {
            slot.priority = new;
        }
    }

    /// Drop every queued frame for `id`, unscheduling it and freeing the
    /// capacity they held. Used when a stream is reset: any STREAM data still
    /// buffered here must not reach the wire *after* the RESET_STREAM, or it's
    /// post-terminal data that wastes congestion window on a stream the peer has
    /// already abandoned. No-op if the stream has nothing queued.
    pub fn remove(&self, id: StreamId) {
        let mut inner = self.inner.lock().unwrap();
        let Some(slot) = inner.streams.remove(&id) else {
            return;
        };
        let removed = slot.frames.len();

        // Unschedule it from its band (it's scheduled iff it had frames, which it
        // did). `slot.priority` is the single source of truth for which band.
        if let Some(queue) = inner.bands.get_mut(&slot.priority) {
            if let Some(pos) = queue.iter().position(|&s| s == id) {
                queue.remove(pos);
            }
            if queue.is_empty() {
                inner.bands.remove(&slot.priority);
            }
        }

        inner.len -= removed;
        drop(inner);
        // Freed `removed` slots; wake that many blocked producers (mirrors the
        // per-slot notify in `pop_locked`).
        for _ in 0..removed {
            self.has_space.notify_one();
        }
    }

    /// Close the queue, unblocking any blocked producers and the consumer.
    pub fn close(&self) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.closed = true;
        }
        self.non_empty.notify_waiters();
        self.has_space.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    use crate::proto::Stream;
    use crate::{StreamDir, StreamId};

    fn sid(index: u64) -> StreamId {
        StreamId::new(index, StreamDir::Uni, false)
    }

    fn frame(id: StreamId, tag: u8) -> Frame {
        Frame::Stream(Stream {
            id,
            data: Bytes::copy_from_slice(&[tag]),
            fin: false,
        })
    }

    fn tag_of(frame: &Frame) -> u8 {
        match frame {
            Frame::Stream(s) => s.data[0],
            _ => panic!("expected stream frame"),
        }
    }

    fn id_of(frame: &Frame) -> StreamId {
        match frame {
            Frame::Stream(s) => s.id,
            _ => panic!("expected stream frame"),
        }
    }

    #[tokio::test]
    async fn higher_priority_first() {
        let q = PriorityQueue::new(8);
        let lo = sid(0);
        let hi = sid(1);

        q.push(10, lo, frame(lo, b'l')).await.unwrap();
        q.push(200, hi, frame(hi, b'h')).await.unwrap();

        // High priority drains first.
        assert_eq!(tag_of(&q.pop().await.unwrap()), b'h');
        assert_eq!(tag_of(&q.pop().await.unwrap()), b'l');
    }

    #[tokio::test]
    async fn equal_priority_round_robin() {
        let q = PriorityQueue::new(8);
        let a = sid(0);
        let b = sid(1);

        q.push(5, a, frame(a, 1)).await.unwrap();
        q.push(5, a, frame(a, 2)).await.unwrap();
        q.push(5, b, frame(b, 1)).await.unwrap();
        q.push(5, b, frame(b, 2)).await.unwrap();

        // Round-robin between equal-priority streams: a, b, a, b.
        assert_eq!(id_of(&q.pop().await.unwrap()), a);
        assert_eq!(id_of(&q.pop().await.unwrap()), b);
        assert_eq!(id_of(&q.pop().await.unwrap()), a);
        assert_eq!(id_of(&q.pop().await.unwrap()), b);
    }

    #[tokio::test]
    async fn per_stream_fifo_preserved() {
        let q = PriorityQueue::new(8);
        let a = sid(0);

        for i in 0..4u8 {
            q.push(5, a, frame(a, i)).await.unwrap();
        }
        for i in 0..4u8 {
            assert_eq!(tag_of(&q.pop().await.unwrap()), i);
        }
    }

    #[tokio::test]
    async fn set_priority_moves_pointer_not_frames() {
        let q = PriorityQueue::new(8);
        let lo = sid(0);
        let hi = sid(1);

        // Two frames each, lo enqueued first.
        q.push(10, lo, frame(lo, 1)).await.unwrap();
        q.push(10, lo, frame(lo, 2)).await.unwrap();
        q.push(20, hi, frame(hi, 1)).await.unwrap();

        // Promote lo above hi; its frames keep their order.
        q.set_priority(lo, 100);

        assert_eq!(id_of(&q.pop().await.unwrap()), lo);
        assert_eq!(tag_of(&q.pop().await.unwrap()), 2); // lo's second frame, in order
        assert_eq!(id_of(&q.pop().await.unwrap()), hi);
    }

    #[tokio::test]
    async fn set_priority_unknown_stream_is_noop() {
        let q = PriorityQueue::new(8);
        q.set_priority(sid(99), 50); // must not panic
    }

    #[tokio::test]
    async fn close_unblocks_pop() {
        let q = PriorityQueue::new(8);
        let q2 = q.clone();
        let handle = tokio::spawn(async move { q2.pop().await });
        tokio::task::yield_now().await;
        q.close();
        assert!(handle.await.unwrap().is_none());
    }

    #[tokio::test]
    async fn close_unblocks_push() {
        let q = PriorityQueue::new(1);
        let a = sid(0);
        q.push(5, a, frame(a, 1)).await.unwrap();

        let q2 = q.clone();
        let handle = tokio::spawn(async move { q2.push(5, sid(1), frame(sid(1), 2)).await });
        tokio::task::yield_now().await;
        q.close();
        assert!(matches!(handle.await.unwrap(), Err(Error::Closed)));
    }

    #[tokio::test]
    async fn backpressure_blocks_at_capacity() {
        let q = PriorityQueue::new(2);
        let a = sid(0);
        q.push(5, a, frame(a, 1)).await.unwrap();
        q.push(5, a, frame(a, 2)).await.unwrap();

        let q2 = q.clone();
        let pushing = tokio::spawn(async move { q2.push(5, a, frame(a, 3)).await });
        tokio::task::yield_now().await;
        assert!(!pushing.is_finished(), "push should block while full");

        // Draining one frame makes room.
        q.pop().await.unwrap();
        pushing.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn remove_drops_a_streams_queued_frames() {
        let q = PriorityQueue::new(8);
        let a = sid(0);
        let b = sid(1);

        q.push(5, a, frame(a, 1)).await.unwrap();
        q.push(5, a, frame(a, 2)).await.unwrap();
        q.push(5, b, frame(b, 9)).await.unwrap();

        // Reset `a`: its queued frames vanish, `b`'s survive.
        q.remove(a);

        assert_eq!(id_of(&q.pop().await.unwrap()), b);
        // Nothing left: the next pop blocks, so a closed-drain returns None.
        q.close();
        assert!(q.pop().await.is_none());
    }

    #[tokio::test]
    async fn remove_frees_capacity_for_blocked_producers() {
        let q = PriorityQueue::new(2);
        let a = sid(0);
        let b = sid(1);
        q.push(5, a, frame(a, 1)).await.unwrap();
        q.push(5, a, frame(a, 2)).await.unwrap();

        // Queue is full; a push for `b` blocks.
        let q2 = q.clone();
        let pushing = tokio::spawn(async move { q2.push(5, b, frame(b, 1)).await });
        tokio::task::yield_now().await;
        assert!(!pushing.is_finished(), "push should block while full");

        // Removing `a` frees both slots and wakes the blocked producer.
        q.remove(a);
        pushing.await.unwrap().unwrap();
        assert_eq!(id_of(&q.pop().await.unwrap()), b);
    }

    #[tokio::test]
    async fn remove_unknown_stream_is_noop() {
        let q = PriorityQueue::new(8);
        q.remove(sid(99)); // must not panic or underflow
    }
}
