use tokio_quiche::socket::SocketCapabilities;

/// Enable the socket options tokio-quiche knows how to use, optionally leaving
/// `UDP_SEGMENT` (GSO) off.
///
/// All of these are Linux-only; every other platform reports no capabilities.
#[cfg(target_os = "linux")]
pub(super) fn capabilities<S: std::os::fd::AsFd>(socket: &S, gso: bool) -> SocketCapabilities {
    use tokio_quiche::socket::SocketCapabilitiesBuilder;

    if gso {
        return SocketCapabilities::apply_all_and_get_compatibility(socket);
    }

    // `apply_all_and_get_compatibility` always turns GSO on, so opting out means
    // reproducing it here minus the `gso()` call. Each option is independently
    // best-effort: a kernel that rejects one still gets the rest.
    let mut builder = SocketCapabilitiesBuilder::new(socket);
    let _ = builder.check_udp_drop();
    let _ = builder.txtime();
    let _ = builder.gro();
    let _ = builder.rcvmark();
    let _ = builder.ip_mtu_discover_probe();
    let _ = builder.ipv6_mtu_discover_probe();

    // Source-address control is only useful when the socket may send from an
    // address it isn't bound to.
    if let Ok(true) = builder.allows_nonlocal_source() {
        let _ = builder.ipv4_pktinfo();
        let _ = builder.ipv4_recvorigdstaddr();
        let _ = builder.ipv6_pktinfo();
        let _ = builder.ipv6_recvorigdstaddr();
    }

    builder.finish()
}

#[cfg(not(target_os = "linux"))]
pub(super) fn capabilities<S>(_socket: &S, _gso: bool) -> SocketCapabilities {
    SocketCapabilities::default()
}
