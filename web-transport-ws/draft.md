---
title: "WebTransport over WebSocket"
abbrev: "WT-WS"
docname: draft-lcurley-wt-ws-00
category: exp
submissiontype: IETF

author:
  -
    fullname: Luke Curley
    email: kixelated@gmail.com

--- abstract

This document defines a protocol for multiplexing bidirectional and
unidirectional streams over a WebSocket connection, providing a
WebTransport-compatible interface for environments where QUIC is
unavailable. The protocol reuses a subset of QUIC frame encodings
(RFC 9000) carried as binary WebSocket messages, with simplifications
enabled by the reliable, ordered transport that WebSocket provides.

--- middle

# Introduction

WebTransport [WebTransport] provides a general-purpose, multiplexed
transport API for web clients. The primary instantiation runs over
HTTP/3 and QUIC, but not all networks and endpoints support UDP.
This document specifies a fallback that carries the WebTransport
stream abstraction over a WebSocket connection [RFC 6455].

The design reuses QUIC variable-length integer encoding and a subset
of QUIC frame types so that implementations can share parsing code
with a native QUIC stack. Fields that are redundant when carried
inside a reliable, ordered, message-oriented transport (offsets,
lengths, final sizes) are omitted.

This protocol is related to QMux [QMux], which also multiplexes
QUIC-style streams over a reliable transport. The key difference is
that this protocol is purpose-built for WebSocket and targets the
WebTransport API surface.

## Conventions and Definitions

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT",
"SHOULD", "SHOULD NOT", "RECOMMENDED", "NOT RECOMMENDED", "MAY", and
"OPTIONAL" in this document are to be interpreted as described in
BCP 14 [RFC 2119] [RFC 8174] when, and only when, they appear in all
capitals, as shown here.

# Protocol Overview

A session is established by opening a WebSocket connection with the
subprotocol token `webtransport` negotiated via the
`Sec-WebSocket-Protocol` header [RFC 6455, Section 4.2.2].

All protocol frames are carried as binary WebSocket messages. Each
binary message contains exactly one frame. Text messages MUST NOT be
sent; a receiver that receives a text message SHOULD close the
connection with a WebSocket status code of 1002 (Protocol Error).

## URL Scheme Mapping

When a WebTransport client is given an `https` URL, the
implementation converts the scheme to `wss` before initiating the
WebSocket handshake. Likewise, `http` is converted to `ws`. The
host, port, path, and query components are preserved unchanged.

# Variable-Length Integer Encoding

This protocol uses the variable-length integer encoding defined in
Section 16 of [RFC 9000]. The encoding uses the two most-significant
bits of the first byte to indicate the length of the integer:

~~~
+------+--------+-------------+-----------------------+
| 2MSB | Length | Usable Bits | Range                 |
+------+--------+-------------+-----------------------+
| 00   | 1      | 6           | 0-63                  |
| 01   | 2      | 14          | 0-16383               |
| 10   | 4      | 30          | 0-1073741823          |
| 11   | 8      | 62          | 0-4611686018427387903 |
+------+--------+-------------+-----------------------+
~~~

All integer fields in the frames defined in this document use this
encoding unless stated otherwise.

# Stream Identifiers

Stream identifiers follow the convention defined in Section 2.1 of
[RFC 9000]. A stream ID is a variable-length integer whose two
least-significant bits encode the stream type:

~~~
+------+-------------------------------------+
| Bits | Stream Type                         |
+------+-------------------------------------+
| 0x00 | Client-Initiated, Bidirectional     |
| 0x01 | Server-Initiated, Bidirectional     |
| 0x02 | Client-Initiated, Unidirectional    |
| 0x03 | Server-Initiated, Unidirectional    |
+------+-------------------------------------+
~~~

The remaining bits (above the two least-significant) form a
monotonically increasing sequence number starting at zero. For
example, the first client-initiated bidirectional stream has ID 0x00,
the second has ID 0x04, the third 0x08, and so on.

An endpoint MUST NOT send a STREAM or STREAM_FIN frame on a stream
that it is not permitted to send on (e.g., the receive side of a
unidirectional stream initiated by the peer). A receiver MUST treat
such a frame as a connection error.

# Frame Definitions

Each binary WebSocket message contains exactly one frame. A frame
begins with a one-byte type field, followed by type-specific fields.

The frame type is encoded as a single byte (not a variable-length
integer), and only the five types defined below are valid. An
endpoint that receives an unrecognized frame type MUST close the
connection with a protocol error.

## STREAM (0x08) and STREAM_FIN (0x09)

STREAM frames carry application data on a stream. STREAM_FIN
additionally signals the end of the sender's data on that stream.

~~~
STREAM / STREAM_FIN Frame {
  Type (8) = 0x08 | 0x09,
  Stream ID (i),
  Stream Data (..),
}
~~~

Type:

: 0x08 indicates a STREAM frame. 0x09 indicates a STREAM_FIN frame.

Stream ID:

: A variable-length integer indicating the stream on which data is
  being sent.

Stream Data:

: The remaining bytes in the WebSocket message after the Stream ID.
  MAY be empty. A STREAM_FIN frame with empty data signals the end
  of the stream without carrying additional payload.

Unlike QUIC, these frames carry no Offset field because the
underlying WebSocket connection provides ordered delivery. They carry
no Length field because the WebSocket message boundary delimits the
frame.

## RESET_STREAM (0x04)

A RESET_STREAM frame abruptly terminates the sending part of a
stream. The receiver SHOULD discard any data already received on
the stream and signal the error to the application.

~~~
RESET_STREAM Frame {
  Type (8) = 0x04,
  Stream ID (i),
  Application Protocol Error Code (i),
}
~~~

Stream ID:

: A variable-length integer identifying the stream being reset.

Application Protocol Error Code:

: A variable-length integer containing the application-level error
  code indicating why the stream is being reset.

Unlike QUIC, this frame omits the Final Size field because there
is no flow control to reconcile.

## STOP_SENDING (0x05)

A STOP_SENDING frame requests that the peer stop sending data on a
stream. It is sent by the receiver of a stream to signal that
incoming data is no longer needed.

~~~
STOP_SENDING Frame {
  Type (8) = 0x05,
  Stream ID (i),
  Application Protocol Error Code (i),
}
~~~

Stream ID:

: A variable-length integer identifying the stream being stopped.

Application Protocol Error Code:

: A variable-length integer containing the application-level error
  code communicated to the sender.

Upon receiving a STOP_SENDING frame, the sender SHOULD respond with
a RESET_STREAM frame for the same stream, using the same or a
different error code.

## CONNECTION_CLOSE (0x1d)

A CONNECTION_CLOSE frame signals that the peer is closing the
session. This corresponds to the application-level CONNECTION_CLOSE
frame in QUIC (type 0x1d), not the transport-level variant (0x1c).

~~~
CONNECTION_CLOSE Frame {
  Type (8) = 0x1d,
  Error Code (i),
  Reason Phrase (..),
}
~~~

Error Code:

: A variable-length integer containing the error code indicating
  the reason for closing the connection.

Reason Phrase:

: The remaining bytes in the WebSocket message after the Error Code,
  decoded as UTF-8. MAY be empty.

Unlike QUIC, this frame omits the Reason Phrase Length field because
the WebSocket message boundary delimits the frame. It also omits the
Frame Type field (present in QUIC's CONNECTION_CLOSE) since there is
no transport-level frame processing to attribute errors to.

# Stream Lifecycle

## Opening a Stream

A stream is implicitly opened when the first STREAM or STREAM_FIN
frame with a new stream ID is sent or received. There is no explicit
stream-open handshake. The initiator selects the next unused stream
ID of the appropriate type (bidirectional or unidirectional,
client-initiated or server-initiated).

Stream IDs within each type SHOULD be used sequentially starting
from zero. An endpoint MAY treat a gap in stream IDs as a protocol
error, but this is not required.

## Sending Data

An endpoint sends data on a stream by sending STREAM frames
containing the stream ID and a payload. Multiple STREAM frames MAY
be sent on the same stream; the receiver reassembles them in the
order received.

## Finishing a Stream

An endpoint signals that it has no more data to send on a stream by
sending a STREAM_FIN frame (type 0x09). The STREAM_FIN frame MAY
contain a final payload or MAY have an empty payload. After sending
STREAM_FIN, the endpoint MUST NOT send further STREAM or STREAM_FIN
frames on that stream.

For bidirectional streams, finishing one direction does not affect the
other; each direction is closed independently.

## Resetting a Stream

An endpoint may abruptly terminate a stream by sending a
RESET_STREAM frame. This signals to the receiver that any buffered
data should be discarded. After sending RESET_STREAM, the sender
MUST NOT send further STREAM frames on that stream.

## Stopping a Stream

An endpoint may indicate it is no longer interested in receiving data
on a stream by sending a STOP_SENDING frame. Upon receiving
STOP_SENDING, the sender SHOULD respond with RESET_STREAM.

## Implicit Reset on Drop

If a stream is discarded by the application without being explicitly
finished (via STREAM_FIN) or reset (via RESET_STREAM), the
implementation SHOULD send a RESET_STREAM frame with error code 0
to inform the peer. Similarly, if a receive stream is dropped
without being fully consumed, the implementation SHOULD send a
STOP_SENDING frame with error code 0.

# Connection Lifecycle

## Establishing a Session

The client initiates a WebSocket handshake [RFC 6455] to the target
URL, including `webtransport` in the `Sec-WebSocket-Protocol` header.
The server validates the subprotocol and includes `webtransport` in
the response header to confirm.

If the server does not select the `webtransport` subprotocol, the
client MUST treat the connection as failed.

After the WebSocket handshake completes, both endpoints may
immediately begin opening streams and sending frames.

## Graceful Close

An endpoint initiates a graceful close by sending a CONNECTION_CLOSE
frame with an appropriate error code and optional reason phrase. After
sending CONNECTION_CLOSE, the endpoint SHOULD close the underlying
WebSocket connection.

Upon receiving a CONNECTION_CLOSE frame, an endpoint SHOULD close the
WebSocket connection. All active streams are implicitly terminated.

## WebSocket Close

If the WebSocket connection is closed (whether by a WebSocket Close
frame or a TCP disconnect) without a preceding CONNECTION_CLOSE
frame, the session is considered abruptly terminated. All active
streams are implicitly reset.

## Error Handling

An endpoint that receives an invalid frame (unrecognized frame type,
truncated variable-length integer, stream ID that violates
directionality rules) SHOULD send a CONNECTION_CLOSE frame with an
appropriate error code and then close the WebSocket connection.

# Differences from QUIC

This protocol deliberately omits the following QUIC features, as
they are either unnecessary or handled by the underlying WebSocket
transport:

Flow Control:

: WebSocket runs over TCP, which provides its own flow control.
  This protocol does not implement QUIC-level flow control (no
  MAX_DATA, MAX_STREAM_DATA, MAX_STREAMS, or DATA_BLOCKED frames).
  Note: the absence of application-level flow control means a fast
  sender can cause unbounded buffering at the receiver (see
  {{security-considerations}}).

Acknowledgments and Loss Recovery:

: TCP handles reliable delivery. No ACK or retransmission logic
  is needed.

Congestion Control:

: TCP provides congestion control. No additional mechanism is
  defined.

Connection IDs and Migration:

: WebSocket connections are bound to a single TCP connection.
  Connection migration is not supported.

0-RTT and Connection Resumption:

: Not applicable; the WebSocket handshake establishes the
  connection.

QUIC Transport Parameters:

: Not exchanged. The protocol has no negotiable parameters beyond
  the WebSocket subprotocol.

Offset and Length Fields:

: STREAM frames omit offset fields because WebSocket messages
  arrive in order. Length fields are omitted because WebSocket
  message framing provides boundaries.

Final Size:

: RESET_STREAM omits the Final Size field because there is no flow
  control state to reconcile.

Datagrams:

: This protocol does not currently support unreliable datagrams.
  All data is carried on streams and delivered reliably.

# Security Considerations

## Transport Security

This protocol relies entirely on WebSocket transport security. When
using `wss` URLs, the WebSocket connection runs over TLS, providing
confidentiality and integrity. Implementations SHOULD use `wss`
(TLS) in production environments.

## Unbounded Buffering

Because this protocol does not implement flow control, a sender can
transmit data faster than a receiver processes it, causing unbounded
memory growth at the receiver. Implementations SHOULD impose
application-level limits on buffer sizes and close the connection if
those limits are exceeded.

## Denial of Service

An attacker could open many streams without sending data or sending
data at a very slow rate. Implementations SHOULD limit the number
of concurrent streams and apply timeouts to idle streams.

## Stream ID Exhaustion

Stream IDs are encoded as variable-length integers with a maximum
value of 2^62-1. In practice, exhaustion is not a concern for
individual connections, but implementations SHOULD handle the
case gracefully.

# IANA Considerations

## WebSocket Subprotocol Name Registration

This document registers the following WebSocket subprotocol name
in the "WebSocket Subprotocol Name Registry" established by
[RFC 6455]:

Subprotocol Identifier:

: webtransport

Subprotocol Common Name:

: WebTransport over WebSocket

Subprotocol Definition:

: This document

--- back

# Acknowledgments

This protocol is inspired by QMux [QMux] and the QUIC transport
protocol [RFC 9000].

# References

## Normative References

### RFC 2119

Bradner, S., "Key words for use in RFCs to Indicate Requirement
Levels", BCP 14, RFC 2119, DOI 10.17487/RFC2119, March 1997,
<https://www.rfc-editor.org/info/rfc2119>.

### RFC 6455

Fette, I. and A. Melnikov, "The WebSocket Protocol", RFC 6455,
DOI 10.17487/RFC6455, December 2011,
<https://www.rfc-editor.org/info/rfc6455>.

### RFC 8174

Leiba, B., "Ambiguity of Uppercase vs Lowercase in RFC 2119 Key
Words", BCP 14, RFC 8174, DOI 10.17487/RFC8174, May 2017,
<https://www.rfc-editor.org/info/rfc8174>.

### RFC 9000

Iyengar, J., Ed. and M. Thomson, Ed., "QUIC: A UDP-Based
Multiplexed and Secure Transport", RFC 9000,
DOI 10.17487/RFC9000, May 2021,
<https://www.rfc-editor.org/info/rfc9000>.

## Informative References

### WebTransport

Vasiliev, V., "WebTransport over HTTP/3", RFC 9297,
DOI 10.17487/RFC9297, August 2022,
<https://www.rfc-editor.org/info/rfc9297>.

### QMux

Opik, A., "QUIC Multiplexing (QMux)", Internet-Draft
draft-opik-quic-qmux, 2024.
