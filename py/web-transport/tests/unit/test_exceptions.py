import web_transport


def test_base_hierarchy():
    assert issubclass(web_transport.SessionError, web_transport.WebTransportError)
    assert issubclass(web_transport.StreamError, web_transport.WebTransportError)
    assert issubclass(web_transport.DatagramError, web_transport.WebTransportError)


def test_session_error_hierarchy():
    assert issubclass(web_transport.ConnectError, web_transport.SessionError)
    assert issubclass(web_transport.SessionClosed, web_transport.SessionError)
    assert issubclass(web_transport.SessionClosedByPeer, web_transport.SessionClosed)
    assert issubclass(web_transport.SessionClosedLocally, web_transport.SessionClosed)
    assert issubclass(web_transport.SessionTimeout, web_transport.SessionError)
    assert issubclass(web_transport.ProtocolError, web_transport.SessionError)


def test_stream_error_hierarchy():
    assert issubclass(web_transport.StreamClosed, web_transport.StreamError)
    assert issubclass(web_transport.StreamClosedByPeer, web_transport.StreamClosed)
    assert issubclass(web_transport.StreamClosedLocally, web_transport.StreamClosed)
    assert issubclass(web_transport.StreamTooLongError, web_transport.StreamError)
    assert issubclass(
        web_transport.StreamIncompleteReadError, web_transport.StreamError
    )


def test_datagram_error_hierarchy():
    assert issubclass(web_transport.DatagramTooLargeError, web_transport.DatagramError)
    assert issubclass(
        web_transport.DatagramNotSupportedError, web_transport.DatagramError
    )


def test_all_inherit_from_base():
    for exc_name in [
        "SessionError",
        "ConnectError",
        "SessionClosed",
        "SessionClosedByPeer",
        "SessionClosedLocally",
        "SessionTimeout",
        "ProtocolError",
        "StreamError",
        "StreamClosed",
        "StreamClosedByPeer",
        "StreamClosedLocally",
        "StreamTooLongError",
        "StreamIncompleteReadError",
        "DatagramError",
        "DatagramTooLargeError",
        "DatagramNotSupportedError",
    ]:
        exc = getattr(web_transport, exc_name)
        assert issubclass(exc, web_transport.WebTransportError), (
            f"{exc_name} does not inherit from WebTransportError"
        )
        assert issubclass(exc, Exception), f"{exc_name} does not inherit from Exception"
