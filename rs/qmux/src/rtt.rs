//! Round-trip time estimated from QX_PING request/response pairs.
//!
//! QMux has no acknowledgement machinery of its own, so the only latency signal
//! available is the one the draft already defines: `QX_PING` is a request/response
//! pair whose Sequence Number the responder MUST echo, and it is not reserved for
//! keep-alive use. The timer task allocates sequences and the writer stamps each
//! one as it actually reaches the wire; the reader matches responses back.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

/// The most requests we track before dropping the oldest.
///
/// A peer that answers retires its requests immediately, so this only grows
/// against one that keeps the session alive with data while ignoring `QX_PING`.
/// The dropped entry is a sample we could no longer have matched anyway.
const MAX_OUTSTANDING: usize = 64;

/// Round-trip time estimated from `QX_PING` exchanges.
///
/// Shared by the timer, writer, and reader tasks, and read synchronously by
/// [`Session::stats`](crate::Session). Empty until the first response arrives.
#[derive(Default, Debug)]
pub(crate) struct Rtt {
    state: Mutex<State>,
}

#[derive(Default, Debug)]
struct State {
    /// Requests written but not yet answered, oldest first. Sequences are
    /// allocated in increasing order, so this stays sorted by construction.
    outstanding: VecDeque<(u64, Instant)>,
    /// Smoothed estimate, `None` until the first sample.
    smoothed: Option<Duration>,
}

impl Rtt {
    /// Record that a `QX_PING` request left the wire, timing it from `at`.
    ///
    /// Called by the writer rather than the timer that enqueued it: the control
    /// lane's queueing delay is not part of the peer's round trip, and counting it
    /// would inflate every jitter buffer downstream.
    pub(crate) fn sent(&self, sequence: u64, at: Instant) {
        let mut state = self.state.lock().unwrap();
        if state.outstanding.len() >= MAX_OUTSTANDING {
            state.outstanding.pop_front();
        }
        state.outstanding.push_back((sequence, at));
    }

    /// Match a `QX_PING` response received at `at` and fold in the sample.
    ///
    /// A responder MAY coalesce several outstanding requests into a single response
    /// carrying the largest Sequence Number, so this retires every request up to and
    /// including `sequence`. The sample is measured against the *newest* of them:
    /// that is the one the peer had to receive before it could answer, so the older
    /// entries would each overstate the round trip.
    pub(crate) fn received(&self, sequence: u64, at: Instant) {
        let mut state = self.state.lock().unwrap();

        let mut sent_at = None;
        while let Some(&(seq, when)) = state.outstanding.front() {
            if seq > sequence {
                break;
            }
            sent_at = Some(when);
            state.outstanding.pop_front();
        }

        // Unmatched: a duplicate response, or one whose request we dropped.
        let Some(sent_at) = sent_at else {
            return;
        };
        let sample = at.saturating_duration_since(sent_at);

        // QUIC's SRTT recurrence (RFC 9002): 7/8 of the estimate plus 1/8 sample.
        state.smoothed = Some(match state.smoothed {
            None => sample,
            Some(prev) => (prev * 7 + sample) / 8,
        });
    }

    /// The current smoothed estimate, or `None` before the first response.
    pub(crate) fn get(&self) -> Option<Duration> {
        self.state.lock().unwrap().smoothed
    }
}

/// The connection statistics QMux can report.
///
/// QMux has no congestion controller of its own, so loss and byte counters belong
/// to the transport underneath and are left unreported. Round-trip time it can
/// always measure itself, from `QX_PING`; a send-rate estimate only ever comes
/// from the transport.
pub(crate) struct SessionStats {
    pub(crate) rtt: Option<Duration>,
    pub(crate) estimated_send_rate: Option<u64>,
}

impl web_transport_trait::Stats for SessionStats {
    fn rtt(&self) -> Option<Duration> {
        self.rtt
    }

    fn estimated_send_rate(&self) -> Option<u64> {
        self.estimated_send_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A matched response yields the elapsed time as the first estimate.
    #[test]
    fn single_sample() {
        let rtt = Rtt::default();
        assert_eq!(rtt.get(), None);

        let sent = Instant::now();
        rtt.sent(0, sent);
        rtt.received(0, sent + Duration::from_millis(40));

        assert_eq!(rtt.get(), Some(Duration::from_millis(40)));
    }

    /// A response echoing the largest sequence retires every older request and is
    /// measured against the newest, not the oldest: the peer could not have
    /// answered before it received that one.
    #[test]
    fn coalesced_response_measures_newest() {
        let rtt = Rtt::default();
        let base = Instant::now();

        rtt.sent(0, base);
        rtt.sent(1, base + Duration::from_millis(100));
        rtt.sent(2, base + Duration::from_millis(200));

        // One response for all three, 20ms after the newest went out.
        rtt.received(2, base + Duration::from_millis(220));
        assert_eq!(rtt.get(), Some(Duration::from_millis(20)));

        // All three were retired, so a duplicate matches nothing.
        rtt.received(2, base + Duration::from_secs(10));
        assert_eq!(rtt.get(), Some(Duration::from_millis(20)));
    }

    /// A response for a sequence we never stamped leaves the estimate alone.
    #[test]
    fn unmatched_response_ignored() {
        let rtt = Rtt::default();
        rtt.received(7, Instant::now());
        assert_eq!(rtt.get(), None);
    }

    /// Later samples are smoothed rather than replacing the estimate outright.
    #[test]
    fn smooths_subsequent_samples() {
        let rtt = Rtt::default();
        let base = Instant::now();

        rtt.sent(0, base);
        rtt.received(0, base + Duration::from_millis(80));
        assert_eq!(rtt.get(), Some(Duration::from_millis(80)));

        // 7/8 * 80ms + 1/8 * 160ms = 90ms.
        rtt.sent(1, base + Duration::from_secs(1));
        rtt.received(
            1,
            base + Duration::from_secs(1) + Duration::from_millis(160),
        );
        assert_eq!(rtt.get(), Some(Duration::from_millis(90)));
    }

    /// A peer that keeps the session busy but never answers a ping must not grow
    /// the outstanding list without bound.
    #[test]
    fn outstanding_is_bounded() {
        let rtt = Rtt::default();
        let base = Instant::now();
        for seq in 0..(MAX_OUTSTANDING as u64 * 4) {
            rtt.sent(seq, base + Duration::from_millis(seq));
        }
        assert_eq!(rtt.state.lock().unwrap().outstanding.len(), MAX_OUTSTANDING);
    }
}
