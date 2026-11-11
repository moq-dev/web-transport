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

#[test]
fn rejection_drain_waits_for_acknowledgment_or_peer_stop() {
    for stopped in [false, true] {
        let mut pair = Pair::new();
        let mut driver = driver(true);
        let id = StreamId::CLIENT_BI;
        pair.client
            .stream_send(id.into(), b"request", true)
            .unwrap();
        pair.pump();
        driver.read(&mut pair.server).unwrap();

        // Consume the receive half so collection can report send completion.
        pair.server
            .stream_shutdown(id.into(), quiche::Shutdown::Read, 0)
            .unwrap();
        pair.server
            .stream_send(id.into(), b"response", true)
            .unwrap();
        let (sender, mut receiver) = tokio::sync::oneshot::channel();
        driver.state.lock().drain(id, sender);
        driver.check_drained(&mut pair.server);
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));

        if stopped {
            pair.client
                .stream_shutdown(id.into(), quiche::Shutdown::Read, 42)
                .unwrap();
        }
        // Force two ack-eliciting packets, avoiding an ACK timer or sleeps.
        for _ in 0..2 {
            pair.server.send_ack_eliciting().unwrap();
            pair.pump();
        }
        driver.read(&mut pair.server).unwrap();
        assert!(
            matches!(receiver.try_recv(), Ok(Ok(()))),
            "rejection was not retired (stopped={stopped})"
        );
        assert!(driver.state.lock().draining.is_empty());
    }
}

#[test]
fn cancelled_rejection_drain_is_removed() {
    let mut pair = Pair::new();
    let mut driver = driver(true);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    driver.state.lock().drain(StreamId::CLIENT_BI, sender);
    drop(receiver);
    driver.check_drained(&mut pair.server);
    assert!(driver.state.lock().draining.is_empty());
}

#[tokio::test]
async fn rejection_accepts_a_peer_stop_before_finish() {
    let mut pair = Pair::new();
    let mut driver = driver(true);
    let id = StreamId::CLIENT_BI;
    pair.client
        .stream_send(id.into(), b"request", false)
        .unwrap();
    pair.pump();
    driver.read(&mut pair.server).unwrap();
    let Poll::Ready(Ok((_, send, _recv))) = driver.state.lock().poll_accept_bi(&Waiter::noop())
    else {
        panic!("request was not accepted");
    };
    let mut send = super::super::SendStream::new(id, send, driver.state.clone());
    send.write_all(b"response").await.unwrap();
    driver.flush_send(&mut pair.server, id).unwrap();
    pair.client
        .stream_shutdown(id.into(), quiche::Shutdown::Read, 42)
        .unwrap();
    pair.pump();
    driver.write(&mut pair.server).unwrap();
    assert!(matches!(
        send.finish(),
        Err(super::super::StreamError::Stop(42) | super::super::StreamError::Closed)
    ));
    send.rejected().await.unwrap();
    assert!(driver.state.lock().draining.is_empty());
}

#[test]
fn notification_racing_with_send_retirement_is_harmless() {
    use super::super::SendStream;
    use tracing_subscriber::{layer::SubscriberExt, Layer};

    struct Warnings(Arc<AtomicUsize>);

    impl<S: tracing::Subscriber> Layer<S> for Warnings {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if *event.metadata().level() <= tracing::Level::WARN {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    // Both local terminal paths can retire a stream between notify's two locks.
    for reset in [false, true] {
        let mut pair = Pair::new();
        let mut driver = driver(false);
        let waiter = Waiter::noop();
        assert!(driver.poll(&waiter, &mut pair.client).is_pending());

        let Poll::Ready(Ok((_, retired_id, retired_state))) = driver.state.lock().open_uni(&waiter)
        else {
            panic!("stream was not opened");
        };
        let Poll::Ready(Ok((_, live_id, live_state))) = driver.state.lock().open_uni(&waiter)
        else {
            panic!("stream was not opened");
        };
        let mut retired = SendStream::new(retired_id, retired_state.clone(), driver.state.clone());
        let mut live = SendStream::new(live_id, live_state, driver.state.clone());
        retired.set_priority(1);
        live.set_priority(0);
        assert!(matches!(
            driver.poll(&waiter, &mut pair.client),
            Poll::Ready(Ok(()))
        ));
        driver.write(&mut pair.client).unwrap();
        assert_eq!(driver.state.lock().urgency_of(live_id), Some(1));

        if reset {
            retired.reset(42);
        } else {
            retired.finish().unwrap();
        }

        // Split notify at its lock boundary: it sees an open state, then the
        // driver flushes the queued FIN/reset before the pending ID is enqueued.
        assert!(!retired_state.lock().is_closed());
        assert!(matches!(
            driver.poll(&waiter, &mut pair.client),
            Poll::Ready(Ok(()))
        ));
        assert!(retired_state.lock().is_closed());
        assert!(!driver.send.contains_key(&retired_id));
        let wake = driver.state.lock().send(retired_id).unwrap();
        wake.wake();
        {
            let mut state = driver.state.lock();
            assert!(state.send(retired_id).is_none());
            assert_eq!(state.send.len(), 1, "notifications must coalesce");
        }
        assert!(matches!(
            live.poll_write(&waiter, b"live"),
            Poll::Ready(Ok(4))
        ));
        assert_eq!(driver.state.lock().send.len(), 2);

        let warnings = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(Warnings(warnings.clone()));
        tracing::subscriber::with_default(subscriber, || {
            assert!(matches!(
                driver.poll(&waiter, &mut pair.client),
                Poll::Ready(Ok(()))
            ));
        });
        assert_eq!(
            warnings.load(Ordering::Relaxed),
            0,
            "stale send notification warned"
        );
        assert!(driver.state.lock().send.is_empty());
        assert!(!driver.send.contains_key(&retired_id));
        assert_eq!(driver.state.lock().priority_of(retired_id), None);
        assert_eq!(driver.state.lock().priority_of(live_id), Some(0));
        assert_eq!(driver.state.lock().urgency_of(live_id), Some(0));
        assert!(driver.state.lock().priority.take().is_empty());
        assert!(!driver.state.lock().is_closed());

        pair.pump();
        let mut received = [0; 4];
        assert_eq!(
            pair.server
                .stream_recv(live_id.into(), &mut received)
                .unwrap(),
            (4, false)
        );
        assert_eq!(&received, b"live");

        // A retained handle does not keep notifying, and the driver can sleep
        // again after draining the single stale ID alongside the live work.
        assert!(matches!(
            retired.poll_write(&waiter, b"late"),
            Poll::Ready(Err(_))
        ));
        assert!(driver.state.lock().send.is_empty());
        assert!(driver.poll(&waiter, &mut pair.client).is_pending());
    }
}
