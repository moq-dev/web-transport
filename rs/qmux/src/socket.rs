//! Statistics read from the connection underneath QMux.
//!
//! QMux runs over a reliable byte stream and has no congestion controller of its
//! own, but the transport it sits on usually does. Where that transport is TCP the
//! kernel already tracks a smoothed RTT and (on Linux) a delivery-rate estimate,
//! so the figures are available immediately and cost no extra traffic. This is
//! strictly better than QMux's own [`QX_PING`](crate::Config::rtt_interval)
//! measurement, which needs a round trip before it can report anything.

use std::sync::Arc;
use std::time::Duration;

/// Live statistics for the connection underneath a QMux session.
///
/// Implementations sample the socket on each call and may filter measurements to
/// estimate available capacity. Every metric is optional: `None` means this
/// transport has no estimate for it.
pub trait SocketStats: Send + Sync + 'static {
    /// Smoothed round-trip time, if the transport tracks one.
    fn rtt(&self) -> Option<Duration> {
        None
    }

    /// Estimated available send bandwidth in **bits per second**, if known.
    fn estimated_send_rate(&self) -> Option<u64> {
        None
    }
}

/// A shared handle to a transport's statistics.
pub type SharedSocketStats = Arc<dyn SocketStats>;

/// Windowed maximum of delivery-rate samples, like BBR's bandwidth filter.
///
/// App-limited samples measure the sender rather than the path, so they may only
/// raise the estimate. Only a usable sample can retire old maxima.
#[derive(Debug, Default)]
#[cfg(any(target_os = "linux", test))]
struct RateFilter {
    /// Strictly decreasing rates from front to back; the front is the maximum.
    samples: std::collections::VecDeque<(std::time::Instant, u64)>,
}

#[cfg(any(target_os = "linux", test))]
impl RateFilter {
    const WINDOW: Duration = Duration::from_secs(10);

    fn update(&mut self, now: std::time::Instant, rate: u64, app_limited: bool) -> Option<u64> {
        let expired = |at: &std::time::Instant| now.saturating_duration_since(*at) > Self::WINDOW;
        let usable =
            rate > 0 && (!app_limited || self.samples.front().is_none_or(|(_, max)| rate >= *max));
        // A rejected sample changes nothing, not even expiry: it is no evidence
        // that the path got slower. Zero is no sample at all.
        if usable {
            while self.samples.front().is_some_and(|(at, _)| expired(at)) {
                self.samples.pop_front();
            }
            while self.samples.back().is_some_and(|(_, r)| *r <= rate) {
                self.samples.pop_back();
            }
            self.samples.push_back((now, rate));
        }
        self.samples.front().map(|(_, max)| *max)
    }
}

#[cfg(unix)]
pub use unix::TcpStats;

#[cfg(unix)]
mod unix {
    use super::*;
    use std::io;
    use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};

    /// Kernel TCP statistics for a socket, via `TCP_INFO` (Linux) or
    /// `TCP_CONNECTION_INFO` (macOS).
    ///
    /// Holds its own duplicate of the file descriptor, so it stays readable for as
    /// long as the session needs it no matter what the transport does with the
    /// original. Construct it from the accepted socket and hand it to the transport
    /// with `with_socket_stats`.
    ///
    /// Reports nothing for a non-TCP socket (a Unix socket, say): the `getsockopt`
    /// simply fails and every metric reads `None`.
    #[derive(Debug)]
    pub struct TcpStats {
        fd: OwnedFd,
        #[cfg(target_os = "linux")]
        rate: std::sync::Mutex<RateFilter>,
    }

    impl TcpStats {
        /// Duplicate `socket`'s descriptor and read statistics from the copy.
        pub fn new<S: AsFd>(socket: &S) -> io::Result<Self> {
            Ok(Self::from_fd(socket.as_fd().try_clone_to_owned()?))
        }

        /// Take ownership of an already-duplicated descriptor.
        ///
        /// Use this when the socket itself has been moved somewhere unreachable (a
        /// type-erased HTTP upgrade, for instance) and only the descriptor was kept.
        pub fn from_fd(fd: OwnedFd) -> Self {
            Self {
                fd,
                #[cfg(target_os = "linux")]
                rate: Default::default(),
            }
        }

        fn borrow(&self) -> BorrowedFd<'_> {
            self.fd.as_fd()
        }
    }

    #[cfg(target_os = "linux")]
    mod sys {
        use super::*;

        /// `struct tcp_info` from `linux/tcp.h`.
        ///
        /// Mirrored field-for-field rather than hand-computing offsets, so `repr(C)`
        /// alignment places each member where the kernel put it. The kernel reports
        /// how much it actually filled in, which is less than this on older
        /// releases, so every read checks its field against that length.
        #[repr(C)]
        #[derive(Default, Clone, Copy)]
        pub struct TcpInfo {
            pub state: u8,
            pub ca_state: u8,
            pub retransmits: u8,
            pub probes: u8,
            pub backoff: u8,
            pub options: u8,
            pub wscale: u8,
            pub app_limited: u8,

            pub rto: u32,
            pub ato: u32,
            pub snd_mss: u32,
            pub rcv_mss: u32,

            pub unacked: u32,
            pub sacked: u32,
            pub lost: u32,
            pub retrans: u32,
            pub fackets: u32,

            pub last_data_sent: u32,
            pub last_ack_sent: u32,
            pub last_data_recv: u32,
            pub last_ack_recv: u32,

            pub pmtu: u32,
            pub rcv_ssthresh: u32,
            /// Smoothed RTT in microseconds.
            pub rtt: u32,
            pub rttvar: u32,
            pub snd_ssthresh: u32,
            pub snd_cwnd: u32,
            pub advmss: u32,
            pub reordering: u32,

            pub rcv_rtt: u32,
            pub rcv_space: u32,
            pub total_retrans: u32,

            pub pacing_rate: u64,
            pub max_pacing_rate: u64,
            pub bytes_acked: u64,
            pub bytes_received: u64,
            pub segs_out: u32,
            pub segs_in: u32,
            pub notsent_bytes: u32,
            pub min_rtt: u32,
            pub data_segs_out: u32,
            pub data_segs_in: u32,
            /// Delivery-rate estimate in bytes per second (Linux 4.9+).
            pub delivery_rate: u64,
        }

        const IPPROTO_TCP: libc::c_int = 6;
        const TCP_INFO: libc::c_int = 11;

        // `tcpi_delivery_rate_app_limited:1` is the first bitfield in its byte.
        pub const APP_LIMITED: u8 = if cfg!(target_endian = "little") {
            1
        } else {
            1 << 7
        };

        /// Read `tcp_info`, returning it alongside the number of bytes the kernel
        /// filled in. `None` if the socket isn't TCP.
        pub fn tcp_info(fd: BorrowedFd<'_>) -> Option<(TcpInfo, usize)> {
            let mut info = TcpInfo::default();
            let mut len = std::mem::size_of::<TcpInfo>() as libc::socklen_t;

            // SAFETY: `info` is a live, correctly sized allocation and `len` says how
            // large it is; the kernel writes at most that much and updates `len` to
            // what it wrote.
            let rc = unsafe {
                libc::getsockopt(
                    fd.as_raw_fd(),
                    IPPROTO_TCP,
                    TCP_INFO,
                    (&mut info as *mut TcpInfo).cast(),
                    &mut len,
                )
            };

            (rc == 0).then_some((info, len as usize))
        }

        /// Whether the kernel filled in the field at `offset` of size `size`.
        pub fn has_field(len: usize, offset: usize, size: usize) -> bool {
            len >= offset + size
        }
    }

    #[cfg(target_os = "macos")]
    mod sys {
        use super::*;

        /// `struct tcp_connection_info` from macOS `netinet/tcp.h`.
        #[repr(C)]
        #[derive(Default, Clone, Copy)]
        pub struct TcpInfo {
            pub state: u8,
            pub snd_wscale: u8,
            pub rcv_wscale: u8,
            _pad1: u8,
            pub options: u32,
            pub flags: u32,
            pub rto: u32,
            pub maxseg: u32,
            pub snd_ssthresh: u32,
            pub snd_cwnd: u32,
            pub snd_wnd: u32,
            pub snd_sbbytes: u32,
            pub rcv_wnd: u32,
            /// Most recent RTT sample, in milliseconds.
            pub rttcur: u32,
            /// Smoothed RTT in milliseconds.
            pub srtt: u32,
            pub rttvar: u32,
        }

        const IPPROTO_TCP: libc::c_int = 6;
        const TCP_CONNECTION_INFO: libc::c_int = 0x106;

        pub fn tcp_info(fd: BorrowedFd<'_>) -> Option<(TcpInfo, usize)> {
            let mut info = TcpInfo::default();
            let mut len = std::mem::size_of::<TcpInfo>() as libc::socklen_t;

            // SAFETY: as for the Linux arm above.
            let rc = unsafe {
                libc::getsockopt(
                    fd.as_raw_fd(),
                    IPPROTO_TCP,
                    TCP_CONNECTION_INFO,
                    (&mut info as *mut TcpInfo).cast(),
                    &mut len,
                )
            };

            (rc == 0).then_some((info, len as usize))
        }

        pub fn has_field(len: usize, offset: usize, size: usize) -> bool {
            len >= offset + size
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    impl SocketStats for TcpStats {
        fn rtt(&self) -> Option<Duration> {
            let (info, len) = sys::tcp_info(self.borrow())?;

            // A successful `getsockopt` on a connected socket is a measurement, so
            // report it even at zero. macOS counts in whole milliseconds, and a
            // loopback round trip genuinely rounds to zero there.
            #[cfg(target_os = "linux")]
            {
                let offset = std::mem::offset_of!(sys::TcpInfo, rtt);
                sys::has_field(len, offset, size_of::<u32>())
                    .then(|| Duration::from_micros(info.rtt as u64))
            }

            #[cfg(target_os = "macos")]
            {
                let offset = std::mem::offset_of!(sys::TcpInfo, srtt);
                sys::has_field(len, offset, size_of::<u32>())
                    .then(|| Duration::from_millis(info.srtt as u64))
            }
        }

        fn estimated_send_rate(&self) -> Option<u64> {
            // Only Linux measures a delivery rate. macOS exposes a congestion window
            // but no rate, and cwnd/rtt is a poor stand-in, so report nothing there
            // rather than a figure callers would mistake for a measurement.
            #[cfg(target_os = "linux")]
            {
                // Serialize sampling as well as updates so concurrent callers
                // cannot insert older measurements after newer ones.
                let mut filter = self
                    .rate
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let (info, len) = sys::tcp_info(self.borrow())?;
                let offset = std::mem::offset_of!(sys::TcpInfo, delivery_rate);
                if !sys::has_field(len, offset, size_of::<u64>()) {
                    return None;
                }
                let app_limited = info.app_limited & sys::APP_LIMITED != 0;
                // The kernel reports bytes per second; the trait wants bits.
                let rate = info.delivery_rate.saturating_mul(8);
                filter.update(std::time::Instant::now(), rate, app_limited)
            }

            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        }
    }

    /// Platforms without a readable TCP info struct report nothing.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    impl SocketStats for TcpStats {}

    /// Pin the field offsets against the kernel's struct.
    ///
    /// The struct is mirrored field-for-field so `repr(C)` computes the offsets,
    /// which means a typo in the middle silently shifts everything after it and we
    /// would read a plausible-looking wrong number rather than fail.
    #[cfg(test)]
    mod tests {
        #[cfg(target_os = "linux")]
        #[test]
        fn zero_kernel_rate_keeps_the_last_estimate() {
            use super::*;
            use std::net::TcpListener;

            // A listener has no ACK-derived samples, regardless of how a kernel
            // accounts for SYNs on connected sockets.
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let stats = TcpStats::new(&listener).unwrap();
            let (info, _) = sys::tcp_info(stats.borrow()).unwrap();
            assert_eq!(info.delivery_rate, 0, "a listener has no rate samples");
            assert_eq!(stats.estimated_send_rate(), None);
            stats
                .rate
                .lock()
                .unwrap()
                .update(std::time::Instant::now(), 80_000_000, false);
            assert_eq!(stats.estimated_send_rate(), Some(80_000_000));
        }

        /// Offsets from `linux/tcp.h`.
        #[cfg(target_os = "linux")]
        #[test]
        fn linux_tcp_info_layout() {
            assert_eq!(std::mem::offset_of!(super::sys::TcpInfo, app_limited), 7);
            #[cfg(target_endian = "little")]
            assert_eq!(super::sys::APP_LIMITED, 0x01);
            #[cfg(target_endian = "big")]
            assert_eq!(super::sys::APP_LIMITED, 0x80);
            assert_eq!(std::mem::offset_of!(super::sys::TcpInfo, rtt), 68);
            assert_eq!(
                std::mem::offset_of!(super::sys::TcpInfo, delivery_rate),
                160
            );
        }

        /// Offsets from macOS `netinet/tcp.h`.
        #[cfg(target_os = "macos")]
        #[test]
        fn macos_tcp_connection_info_layout() {
            assert_eq!(std::mem::offset_of!(super::sys::TcpInfo, srtt), 44);
        }
    }
}

#[cfg(test)]
mod rate_filter_tests {
    use super::*;
    use std::time::Instant;

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn app_limited_samples_only_raise_the_estimate() {
        let t = Instant::now();
        let mut f = RateFilter::default();
        assert_eq!(f.update(t, 1_000_000, true), Some(1_000_000));
        assert_eq!(f.update(at(t, 100), 80_000_000, true), Some(80_000_000));
        assert_eq!(f.update(at(t, 200), 1_000_000, true), Some(80_000_000));
        assert_eq!(f.update(at(t, 20_000), 1_000_000, true), Some(80_000_000));
    }

    #[test]
    fn measured_samples_replace_maxima_only_after_the_window() {
        let t = Instant::now();
        let mut f = RateFilter::default();
        assert_eq!(f.update(t, 80_000_000, false), Some(80_000_000));
        assert_eq!(f.update(at(t, 1_000), 5_000_000, false), Some(80_000_000));
        assert_eq!(
            f.update(t + RateFilter::WINDOW, 4_000_000, false),
            Some(80_000_000)
        );
        assert_eq!(f.update(at(t, 10_001), 3_000_000, false), Some(5_000_000));
    }

    #[test]
    fn rejected_samples_change_nothing_including_expiry() {
        let t = Instant::now();
        let mut f = RateFilter::default();
        f.update(t, 80_000_000, false);
        f.update(at(t, 1_000), 5_000_000, false);
        assert_eq!(f.update(at(t, 20_000), 1_000_000, true), Some(80_000_000));
        assert_eq!(f.update(at(t, 30_000), 1_000_000, true), Some(80_000_000));
        assert_eq!(f.update(at(t, 31_000), 4_000_000, false), Some(4_000_000));
    }

    #[test]
    fn window_keeps_the_largest_recent_measurement() {
        let t = Instant::now();
        let mut f = RateFilter::default();
        f.update(t, 30_000_000, false);
        f.update(at(t, 4_000), 20_000_000, false);
        f.update(at(t, 8_000), 10_000_000, false);
        assert_eq!(f.update(at(t, 10_500), 9_000_000, false), Some(20_000_000));
        assert_eq!(f.update(at(t, 14_500), 9_000_000, false), Some(10_000_000));
        assert_eq!(f.update(at(t, 18_500), 9_000_000, false), Some(9_000_000));
    }

    #[test]
    fn equal_app_limited_sample_refreshes_the_maximum() {
        let t = Instant::now();
        let mut f = RateFilter::default();
        f.update(t, 80_000_000, false);
        assert_eq!(f.update(at(t, 9_000), 80_000_000, true), Some(80_000_000));
        assert_eq!(f.update(at(t, 11_000), 5_000_000, false), Some(80_000_000));
        assert_eq!(f.update(at(t, 19_001), 4_000_000, false), Some(5_000_000));
    }

    #[test]
    fn zero_rate_is_not_a_sample() {
        let t = Instant::now();
        let mut f = RateFilter::default();
        assert_eq!(f.update(t, 0, false), None);
        assert_eq!(f.update(t, 0, true), None);
        f.update(at(t, 100), 80_000_000, false);
        f.update(at(t, 1_000), 5_000_000, false);
        assert_eq!(f.update(at(t, 20_000), 0, false), Some(80_000_000));
        assert_eq!(f.update(at(t, 30_000), 0, true), Some(80_000_000));
        assert_eq!(f.update(at(t, 31_000), 4_000_000, false), Some(4_000_000));
    }
}
