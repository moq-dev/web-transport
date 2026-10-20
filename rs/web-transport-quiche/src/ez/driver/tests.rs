use super::*;

#[test]
fn handshake_completes_without_an_alpn() {
    // The established flag, not the ALPN, is what resolves the handshake: a
    // connection that negotiates no ALPN must still hand back a Connection
    // rather than wait forever.
    let mut state = DriverState::new(false);
    let waiter = Waiter::noop();

    assert!(state.poll_handshake(&waiter).is_pending());

    state.established = true;
    let _ = state.complete_handshake();

    assert!(state.alpn().is_none());
    assert!(matches!(state.poll_handshake(&waiter), Poll::Ready(Ok(()))));
}

#[test]
fn closed_waits_for_driver_completion() {
    let mut state = DriverState::new(false);
    let waiter = Waiter::noop();
    let err = ConnectionError::Local(42, "done".to_string());

    assert!(state.closed(&waiter).is_pending());

    let _ = state.close(err.clone());

    assert!(state.close_requested(&waiter).is_ready());
    assert!(state.closed(&waiter).is_pending());

    let _ = state.finish_close(err);

    assert!(state.closed(&waiter).is_ready());
}

/// A socket-free QUIC pair. Packet delivery and driver callbacks are separate so
/// tests can put the transport into a precise state before the driver sees it.
struct Pair {
    client: QuicheConnection,
    server: QuicheConnection,
}

impl Pair {
    fn new() -> Self {
        use boring::{
            pkey::PKey,
            ssl::{SslContextBuilder, SslMethod},
            x509::X509,
        };

        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let mut tls = SslContextBuilder::new(SslMethod::tls()).unwrap();
        tls.set_certificate(&X509::from_der(cert.cert.der()).unwrap())
            .unwrap();
        tls.set_private_key(
            &PKey::private_key_from_der(&cert.signing_key.serialize_der()).unwrap(),
        )
        .unwrap();

        let mut server_config =
            quiche::Config::with_boring_ssl_ctx_builder(quiche::PROTOCOL_VERSION, tls).unwrap();
        let mut client_config = quiche::Config::new(quiche::PROTOCOL_VERSION).unwrap();
        client_config.verify_peer(false);
        for config in [&mut client_config, &mut server_config] {
            config.set_application_protos(&[b"driver-test"]).unwrap();
            config.set_initial_max_data(1 << 20);
            config.set_initial_max_stream_data_bidi_local(1 << 16);
            config.set_initial_max_stream_data_bidi_remote(1 << 16);
            config.set_initial_max_stream_data_uni(1 << 16);
            config.set_initial_max_streams_bidi(16);
            config.set_initial_max_streams_uni(16);
            config.enable_pacing(false);
        }

        let client_addr = "127.0.0.1:10000".parse().unwrap();
        let server_addr = "127.0.0.1:10001".parse().unwrap();
        let client_id = quiche::ConnectionId::from_ref(b"client-driver-test");
        let server_id = quiche::ConnectionId::from_ref(b"server-driver-test");
        let client = quiche::connect_with_buffer_factory(
            Some("localhost"),
            &client_id,
            client_addr,
            server_addr,
            &mut client_config,
        )
        .unwrap();
        let server = quiche::accept_with_buf_factory(
            &server_id,
            None,
            server_addr,
            client_addr,
            &mut server_config,
        )
        .unwrap();

        let mut pair = Self { client, server };
        pair.pump();
        assert!(pair.client.is_established(), "client handshake stalled");
        assert!(pair.server.is_established(), "server handshake stalled");
        pair
    }

    /// Deliver queued packets immediately without polling an application driver,
    /// waiting for ACK timers, or advancing wall-clock time.
    fn pump(&mut self) {
        for _ in 0..32 {
            let forward = Self::transfer(&mut self.client, &mut self.server);
            let reverse = Self::transfer(&mut self.server, &mut self.client);
            if forward + reverse == 0 {
                return;
            }
        }
        panic!("in-memory packet exchange did not quiesce");
    }

    fn transfer(from: &mut QuicheConnection, to: &mut QuicheConnection) -> usize {
        let mut packet = [0; 1350];
        for sent in 0..256 {
            let (len, info) = match from.send(&mut packet) {
                Ok(sent) => sent,
                Err(quiche::Error::Done) => return sent,
                Err(err) => panic!("packet send failed: {err}"),
            };
            let received = to
                .recv(
                    &mut packet[..len],
                    quiche::RecvInfo {
                        from: info.from,
                        to: info.to,
                    },
                )
                .expect("packet receive failed");
            assert_eq!(received, len);
        }
        panic!("in-memory sender did not quiesce");
    }
}

fn driver(server: bool) -> Driver {
    let (_, dgram_out) = flume::unbounded();
    Driver::new(
        Lock::new(DriverState::new(server)),
        dgram_out,
        Arc::new(AtomicUsize::new(0)),
        None,
    )
}

#[test]
fn accepting_a_stopped_send_half_does_not_register_it() {
    let mut pair = Pair::new();
    let mut driver = driver(true);
    let id = StreamId::CLIENT_BI;
    let code = 42;

    pair.client
        .stream_send(id.into(), b"request", false)
        .unwrap();
    pair.client
        .stream_shutdown(id.into(), quiche::Shutdown::Read, code)
        .unwrap();
    pair.pump();

    // Both STREAM and STOP_SENDING have reached quiche, but no driver callback
    // has accepted the stream yet. Prove that setup before exercising the guard.
    assert!(pair.server.stream_readable(id.into()));
    assert!(matches!(
        pair.server.stream_capacity(id.into()),
        Err(quiche::Error::StreamStopped(42))
    ));
    driver.read(&mut pair.server).unwrap();

    let Poll::Ready(Ok((accepted, send, _recv))) =
        driver.state.lock().poll_accept_bi(&Waiter::noop())
    else {
        panic!("bidirectional stream was not accepted");
    };
    assert_eq!(accepted, id);
    assert!(send.lock().is_closed());
    assert!(matches!(
        send.lock().is_finished(),
        Err(super::super::StreamError::Stop(42))
    ));
    assert!(!driver.send.contains_key(&id), "closed send state leaked");
    assert_eq!(driver.state.lock().priority_of(id), None);
    assert!(!driver.state.lock().priority.take().contains_key(&id));

    // A later driver pass cannot retire a send half quiche no longer reports as
    // writable. It must have been excluded on the initial accept.
    driver.write(&mut pair.server).unwrap();
    assert!(!driver.send.contains_key(&id));
    assert_eq!(driver.state.lock().priority_of(id), None);
    assert!(!driver.state.lock().is_closed());
}

#[test]
fn accepting_a_live_send_half_registers_it() {
    let mut pair = Pair::new();
    let mut driver = driver(true);
    let id = StreamId::CLIENT_BI;

    pair.client
        .stream_send(id.into(), b"request", false)
        .unwrap();
    pair.pump();
    driver.read(&mut pair.server).unwrap();

    let Poll::Ready(Ok((accepted, send, _recv))) =
        driver.state.lock().poll_accept_bi(&Waiter::noop())
    else {
        panic!("bidirectional stream was not accepted");
    };
    assert_eq!(accepted, id);
    assert!(!send.lock().is_closed());
    assert!(driver.send.contains_key(&id));
    assert_eq!(driver.state.lock().priority_of(id), Some(0));
    driver.apply_priorities(&mut pair.server).unwrap();

    pair.server
        .stream_send(id.into(), b"response", true)
        .unwrap();
    pair.pump();
    let mut response = [0; 8];
    assert_eq!(
        pair.client.stream_recv(id.into(), &mut response).unwrap(),
        (response.len(), true)
    );
    assert_eq!(&response, b"response");
}
