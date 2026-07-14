import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { SessionError } from "./error.ts";
import * as Frame from "./frame.ts";
import { DEFAULT_MAX_RECORD_SIZE, type TransportParams } from "./frame.ts";
import Session, { type Config } from "./session.ts";
import * as Stream from "./stream.ts";
import { VarInt } from "./varint.ts";

// A scripted peer standing in for the `WebSocketStream` transport. The test
// plays the remote end: it injects frames into the Session's readable and
// captures frames the Session writes. Installed as `globalThis.WebSocketStream`
// so `openWebSocketStream` (which Session uses) picks it up instead of the real
// ponyfill (it's a distinct class, so the `Native !== WebSocketStream` guard
// holds).
class FakePeer {
	static last: FakePeer | undefined;

	readonly url: string;
	readonly opened: Promise<{
		readable: ReadableStream<Uint8Array | string>;
		writable: WritableStream<Uint8Array>;
		protocol: string;
		extensions: string;
	}>;
	readonly closed: Promise<{ closeCode?: number; reason?: string }>;
	#closedResolve: (info: { closeCode?: number; reason?: string }) => void;
	#closedReject: (err: Error) => void;
	#recv!: ReadableStreamDefaultController<Uint8Array | string>;
	#writeBlocked: Promise<void> | undefined;
	#unblockWrites: (() => void) | undefined;

	/** Raw chunks the Session has written. */
	sent: Uint8Array[] = [];
	closeInfo: { closeCode?: number; reason?: string } | undefined;

	constructor(url: string) {
		this.url = url;
		FakePeer.last = this;
		const readable = new ReadableStream<Uint8Array | string>({
			start: (c) => {
				this.#recv = c;
			},
		});
		const writable = new WritableStream<Uint8Array>({
			write: async (chunk) => {
				await this.#writeBlocked;
				this.sent.push(chunk);
			},
		});
		this.opened = Promise.resolve({ readable, writable, protocol: "qmux-01", extensions: "" });
		const closed = Promise.withResolvers<{ closeCode?: number; reason?: string }>();
		this.closed = closed.promise;
		this.#closedResolve = closed.resolve;
		this.#closedReject = closed.reject;
		// A real WebSocketStream's `closed` can reject; nothing may await it here
		// until the test does, so keep that from surfacing as an unhandled rejection.
		this.closed.catch(() => {});
	}

	close(info: { closeCode?: number; reason?: string } = {}) {
		this.closeInfo = info;
		this.#closedResolve(info);
	}
	/** The socket errored out, rather than closing — `WebSocketStream.closed` rejects. */
	error(err = new Error("socket error")) {
		this.#closedReject(err);
	}
	setHighWaterMark() {}
	blockWrites() {
		const blocked = Promise.withResolvers<void>();
		this.#writeBlocked = blocked.promise;
		this.#unblockWrites = blocked.resolve;
	}
	unblockWrites() {
		this.#unblockWrites?.();
		this.#writeBlocked = undefined;
		this.#unblockWrites = undefined;
	}

	// --- test controls ---
	/** Inject a frame as if sent by the peer. */
	send(frame: Frame.Any) {
		this.#recv.enqueue(Frame.encode(frame, "qmux-01"));
	}
	/** Inject a raw text frame (invalid for QMux). */
	sendText(text: string) {
		this.#recv.enqueue(text);
	}
	/** Inject a raw binary record — bytes the encoder wouldn't normally emit
	 * (e.g. the no-length 0x30 datagram form). */
	sendRaw(bytes: Uint8Array) {
		this.#recv.enqueue(bytes);
	}
	/** All frames the Session has written so far, decoded. */
	received(): Frame.Any[] {
		return this.sent.flatMap((b) => Frame.decodeRecord(b));
	}
	has(type: Frame.Any["type"]): boolean {
		return this.received().some((f) => f.type === type);
	}
	count(type: Frame.Any["type"]): number {
		return this.received().filter((f) => f.type === type).length;
	}
}

/** Poll an observable condition with a bounded timeout (no fixed-yield sleeps). */
async function waitFor(check: () => boolean, timeoutMs = 1000): Promise<void> {
	const start = Date.now();
	while (!check()) {
		if (Date.now() - start > timeoutMs) throw new Error("timed out waiting for condition");
		await new Promise((resolve) => setTimeout(resolve, 0));
	}
}

/** Yield a few macrotask turns so queued reads and any pending promise rejections
 * settle (an unhandled rejection surfaces at the end of the current turn). */
async function settle(turns = 5): Promise<void> {
	for (let i = 0; i < turns; i++) {
		await new Promise((resolve) => setTimeout(resolve, 0));
	}
}

/** Drain queued microtasks so a just-issued claim() reaches its parked state.
 *  Parking is pure-microtask work (no I/O), so a bounded drain is deterministic. */
async function flushMicrotasks(turns = 10): Promise<void> {
	for (let i = 0; i < turns; i++) await Promise.resolve();
}

/** Settle a promise to its value or its rejection error, without leaking an
 *  unhandled rejection while the test arranges the teardown that resolves it. */
function settleTo<T>(p: Promise<T>): Promise<T | Error> {
	return p.then(
		(v) => v,
		(e) => e,
	);
}

/** The session-close frame the Session put on the wire, if any. A locally-detected
 *  violation still owes the peer an explanation, even though it settles `closed`
 *  as a failure on our side. The frame *type* distinguishes the graceful
 *  APPLICATION_CLOSE a `close()` sends from the CONNECTION_CLOSE a violation sends. */
function sentClose(peer: FakePeer): Frame.ApplicationClose | Frame.ConnectionClose | undefined {
	return peer.received().find((f) => f.type === "application_close" || f.type === "connection_close") as
		| Frame.ApplicationClose
		| Frame.ConnectionClose
		| undefined;
}

function sentCloseCode(peer: FakePeer): number | undefined {
	const frame = sentClose(peer);
	return frame ? Number(frame.code.value) : undefined;
}

/** Assert `closed` rejected the way the WebTransport contract requires of an
 *  abnormal end, and hand the error back for further checks. A fulfilled `closed`
 *  is the bug this guards: it makes a dropped session look like a clean shutdown. */
async function expectSessionFailure(session: Session): Promise<SessionError> {
	const settled = await settleTo(session.closed);
	expect(settled).toBeInstanceOf(SessionError);
	const err = settled as SessionError;
	// Shaped like the native WebTransportError so consumers can branch on it.
	expect(err.name).toBe("WebTransportError");
	expect(err.source).toBe("session");
	expect(err.streamErrorCode).toBeNull();
	return err;
}

function peerParams(overrides: Partial<TransportParams> = {}): TransportParams {
	return {
		maxIdleTimeout: 0n,
		initialMaxData: 1_000_000n,
		initialMaxStreamDataBidiLocal: 100_000n,
		initialMaxStreamDataBidiRemote: 100_000n,
		initialMaxStreamDataUni: 100_000n,
		initialMaxStreamsBidi: 100n,
		initialMaxStreamsUni: 100n,
		maxDatagramFrameSize: DEFAULT_MAX_RECORD_SIZE,
		maxRecordSize: DEFAULT_MAX_RECORD_SIZE,
		resetStreamAt: false,
		...overrides,
	};
}

function connect(config?: Config): { session: Session; peer: FakePeer } {
	(globalThis as { WebSocketStream?: unknown }).WebSocketStream = FakePeer;
	// Idle timer off so a stray interval can't interfere with the test.
	// Bare version ALPNs are offered by default (requireProtocol defaults to false).
	const session = new Session("https://example/test", {
		config: { maxIdleTimeout: 0n, ...config },
	});
	const peer = FakePeer.last as FakePeer;
	return { session, peer };
}

const ORIGINAL_WSS = (globalThis as { WebSocketStream?: unknown }).WebSocketStream;

describe("Session integration (scripted peer)", () => {
	afterEach(() => {
		if (ORIGINAL_WSS === undefined) {
			delete (globalThis as { WebSocketStream?: unknown }).WebSocketStream;
		} else {
			(globalThis as { WebSocketStream?: unknown }).WebSocketStream = ORIGINAL_WSS;
		}
	});

	test("handshake: sends TRANSPORT_PARAMETERS and resolves ready", async () => {
		const { session, peer } = connect();
		await session.ready;
		await waitFor(() => peer.has("transport_parameters"));
		session.close();
	});

	test("a uni stream write produces a STREAM frame on the wire", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		// createUnidirectionalStream blocks until the peer's stream-count credit arrives.
		const writable = await session.createUnidirectionalStream();
		await writable.getWriter().write(new Uint8Array([1, 2, 3]));

		await waitFor(() => peer.has("stream"));
		const stream = peer.received().find((f) => f.type === "stream") as Frame.Data;
		expect(stream.data).toEqual(new Uint8Array([1, 2, 3]));
		session.close();
	});

	test("close() sends APPLICATION_CLOSE, resolves closed, and is idempotent", async () => {
		const { session, peer } = connect();
		await session.ready;

		session.close({ closeCode: 42, reason: "bye" });
		expect(await session.closed).toEqual({ closeCode: 42, reason: "bye" });
		await waitFor(() => peer.has("application_close"));
		// A graceful close is an APPLICATION_CLOSE (0x1d), so the peer fulfills.
		expect(sentClose(peer)?.type).toBe("application_close");

		// Second close is a no-op: the resolved info is unchanged.
		session.close({ closeCode: 7, reason: "again" });
		expect(await session.closed).toEqual({ closeCode: 42, reason: "bye" });
	});

	// The WebTransport contract: `closed` fulfills only on a graceful end, and rejects
	// with a WebTransportError (source "session") on an abnormal one. Without that
	// split a consumer cannot tell a dropped session from a clean shutdown — the close
	// code can't carry it, since codes are application-defined.
	describe("closed settles graceful vs abnormal", () => {
		test("a peer APPLICATION_CLOSE resolves closed with its code and reason", async () => {
			const { session, peer } = connect();
			await session.ready;

			// The peer closing the session deliberately (APPLICATION_CLOSE / 0x1d) is
			// graceful, even though we didn't initiate it.
			peer.send({ type: "application_close", code: VarInt.from(42), reason: "bye" });
			expect(await session.closed).toEqual({ closeCode: 42, reason: "bye" });
		});

		test("a peer APPLICATION_CLOSE fulfills even when its code looks like a transport error", async () => {
			const { session, peer } = connect();
			await session.ready;

			// 1002 is a transport-error code, but an APPLICATION_CLOSE is graceful by
			// definition — the *subtype*, not the code, decides. Codes are
			// application-defined, so the app still gets its code and reason.
			peer.send({ type: "application_close", code: VarInt.from(1002), reason: "bye" });
			expect(await session.closed).toEqual({ closeCode: 1002, reason: "bye" });
		});

		test("a peer CONNECTION_CLOSE rejects closed", async () => {
			const { session, peer } = connect();
			await session.ready;

			// The peer detected a protocol violation / transport error and sent a
			// CONNECTION_CLOSE (0x1c) — abnormal, so `closed` rejects rather than
			// fulfilling. Mirrors the frame we emit from #abort.
			peer.send({
				type: "connection_close",
				code: VarInt.from(1002),
				reason: "Protocol violation",
			});
			const err = await expectSessionFailure(session);
			expect(err.message).toContain("Protocol violation");
		});

		test("a socket drop with no CONNECTION_CLOSE rejects closed", async () => {
			const { session, peer } = connect();
			await session.ready;

			// Even a "clean" WebSocket status code is a drop at the session layer: the
			// peer never said goodbye in QMux terms.
			peer.close({ closeCode: 1000, reason: "" });

			const err = await expectSessionFailure(session);
			expect(err.message).toContain("Connection closed");
		});

		test("a socket error rejects closed, preserving the underlying failure", async () => {
			const { session, peer } = connect();
			await session.ready;

			peer.error(new Error("connection reset"));

			// Why the transport died is the whole value of the rejection, so the socket's
			// own error has to survive rather than be flattened into a fixed string.
			const err = await expectSessionFailure(session);
			expect(err.message).toBe("connection reset");
			expect((err.cause as Error).message).toBe("connection reset");
		});

		test("an idle timeout rejects closed when neither direction makes progress", async () => {
			// The sharpest case: an idle timeout used to resolve with { closeCode: 0,
			// reason: "idle timeout" }, while a graceful close() with no args resolves
			// with { closeCode: 0, reason: "" }. A dead peer and a clean shutdown differed
			// only by a free-text string.
			const { session, peer } = connect({ maxIdleTimeout: 150n });
			await session.ready;
			await waitFor(() => peer.has("transport_parameters"));
			// A successfully written keep-alive is send activity under QMux. Block the
			// sink so neither inbound nor outbound frames can reset the idle deadline.
			peer.blockWrites();
			peer.send({ type: "transport_parameters", params: peerParams() });

			const err = await expectSessionFailure(session);
			expect(err.message).toContain("idle timeout");
			peer.unblockWrites();
		});

		test("outbound frame activity resets the idle timeout", async () => {
			const { session, peer } = connect({ maxIdleTimeout: 120n });
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams() });

			const writable = await session.createUnidirectionalStream();
			const writer = writable.getWriter();
			for (let i = 0; i < 6; i++) {
				await new Promise((resolve) => setTimeout(resolve, 45));
				await writer.write(new Uint8Array([i]));
			}

			// More than two idle windows elapsed without another inbound frame, but
			// the successful STREAM writes above kept resetting the shared deadline.
			expect(peer.has("connection_close")).toBe(false);
			session.close();
			expect(await session.closed).toEqual({ closeCode: 0, reason: "" });
		});

		test("the standard reconnect idiom sees a drop", async () => {
			// The downstream bug this fixes (moq-dev/moq#2183): code written against the
			// native API reconnects from the catch arm, which qmux could never reach.
			const { session, peer } = connect();
			await session.ready;

			let reconnected = false;
			const watch = (async () => {
				try {
					await session.closed;
				} catch {
					reconnected = true;
				}
			})();

			peer.close({ closeCode: 1006, reason: "relay bounced" });
			await watch;

			expect(reconnected).toBe(true);
		});
	});

	test("a text frame rejects closed and tells the peer why (1003)", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.sendText("not binary");

		const err = await expectSessionFailure(session);
		expect(err.message).toContain("text frames are not valid for QMux");
		await waitFor(() => sentCloseCode(peer) === 1003);
		// A violation we detect is a CONNECTION_CLOSE (0x1c), so the peer rejects too.
		expect(sentClose(peer)?.type).toBe("connection_close");
	});

	test("an unnegotiated RESET_STREAM_AT rejects closed and tells the peer why (1002)", async () => {
		// The peer negotiates qmux-01, which never advertises `reset_stream_at`, so
		// a RESET_STREAM_AT (0x24) frame is a protocol violation. Hand-built bytes:
		// type=0x24, id=3, code=0, final_size=0, reliable_size=0.
		const { session, peer } = connect();
		await session.ready;
		peer.sendRaw(new Uint8Array([0x24, 0x03, 0x00, 0x00, 0x00]));

		const err = await expectSessionFailure(session);
		// The decode failure itself is carried as the cause, not flattened away.
		expect(err.cause).toBeInstanceOf(Error);
		await waitFor(() => sentCloseCode(peer) === 1002);
		expect(sentClose(peer)?.type).toBe("connection_close");
	});

	test("datagrams: an app write produces a DATAGRAM frame on the wire", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		await waitFor(() => session.datagrams.maxDatagramSize > 0);
		await session.datagrams.writable.getWriter().write(new Uint8Array([1, 2, 3]));

		await waitFor(() => peer.has("datagram"));
		const dg = peer.received().find((f) => f.type === "datagram") as Frame.Datagram;
		expect(Array.from(dg.data)).toEqual([1, 2, 3]);
		session.close();
	});

	test("datagrams: an incoming DATAGRAM frame is delivered to the readable", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		peer.send({ type: "datagram", data: new Uint8Array([9, 8, 7]) });
		const { value } = await session.datagrams.readable.getReader().read();
		expect(Array.from(value as Uint8Array)).toEqual([9, 8, 7]);
		session.close();
	});

	test("datagrams: a DATAGRAM whose frame exactly fits our advertised size is delivered", async () => {
		// ourParams.maxDatagramFrameSize = 10; an 8-byte payload encodes to a
		// 1 (type) + 1 (length varint) + 8 = 10-byte frame — exactly the limit.
		const { session, peer } = connect({ maxDatagramFrameSize: 10n });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		peer.send({ type: "datagram", data: new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]) });
		const { value } = await session.datagrams.readable.getReader().read();
		expect(Array.from(value as Uint8Array)).toEqual([1, 2, 3, 4, 5, 6, 7, 8]);
		session.close();
	});

	test("datagrams: a DATAGRAM whose frame overflows our advertised size is fatal", async () => {
		// ourParams.maxDatagramFrameSize = 10. A 10-byte payload encodes to a
		// 1 + 1 + 10 = 12-byte frame > 10 — a peer exceeding the size we negotiated
		// is a PROTOCOL_VIOLATION (RFC 9221), so the session must close, not drop.
		const { session, peer } = connect({ maxDatagramFrameSize: 10n });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		peer.send({ type: "datagram", data: new Uint8Array(10) });
		await expectSessionFailure(session);
		await waitFor(() => sentCloseCode(peer) === 1002);
	});

	test("datagrams: a DATAGRAM we never advertised support for is fatal", async () => {
		// ourParams.maxDatagramFrameSize = 0 → we advertised no datagram support, so
		// receiving one at all is a PROTOCOL_VIOLATION (RFC 9221), not a droppable frame.
		const { session, peer } = connect({ maxDatagramFrameSize: 0n });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		peer.send({ type: "datagram", data: new Uint8Array([1, 2, 3]) });
		await expectSessionFailure(session);
		await waitFor(() => sentCloseCode(peer) === 1002);
	});

	test("datagrams: a no-length (0x30) DATAGRAM is sized without a length varint", async () => {
		// ourParams.maxDatagramFrameSize = 10. A 0x30 datagram carries no length
		// varint, so a 9-byte payload is a 1 + 9 = 10-byte frame — exactly the
		// limit — even though the length-prefixed reconstruction (1 + 1 + 9 = 11)
		// would wrongly drop it. Hand-build the record: a 0x30 type byte + payload.
		const { session, peer } = connect({ maxDatagramFrameSize: 10n });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const payload = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8, 9]);
		peer.sendRaw(new Uint8Array([0x30, ...payload]));
		const { value } = await session.datagrams.readable.getReader().read();
		expect(Array.from(value as Uint8Array)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9]);
		session.close();
	});

	test("datagrams: maxDatagramSize reflects the peer's advertised frame size", async () => {
		const { session, peer } = connect();
		await session.ready;
		// frame size 1201 → payload 1201 - (1 type byte + 2-byte length varint) = 1198.
		peer.send({ type: "transport_parameters", params: peerParams({ maxDatagramFrameSize: 1201n }) });

		await waitFor(() => session.datagrams.maxDatagramSize > 0);
		expect(session.datagrams.maxDatagramSize).toBe(1198);
		session.close();
	});

	test("datagrams: a peer that disables datagrams leaves maxDatagramSize at 0 and drops writes", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams({ maxDatagramFrameSize: 0n }) });

		// A ping barrier confirms the params have been processed.
		peer.send({ type: "ping_request", sequence: 1n });
		await waitFor(() => peer.has("ping_response"));

		expect(session.datagrams.maxDatagramSize).toBe(0);

		// Writing is dropped, not errored: no DATAGRAM frame reaches the wire.
		await session.datagrams.writable.getWriter().write(new Uint8Array([1, 2, 3]));
		peer.send({ type: "ping_request", sequence: 2n });
		await waitFor(() => peer.count("ping_response") === 2);
		expect(peer.has("datagram")).toBe(false);
		session.close();
	});

	test("datagrams: readable closes cleanly on a graceful session close", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const reader = session.datagrams.readable.getReader();
		session.close(); // graceful: no #closeReason, so the readable must not error
		const { done } = await reader.read();
		expect(done).toBe(true);
	});

	test("MAX_STREAM_DATA is extended on delivery to the app, not on receipt", async () => {
		// Small per-stream window so a few delivered chunks cross the half-window threshold.
		const { session, peer } = connect({ maxStreamDataUni: 1000n });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		// Peer opens a server-initiated uni stream and sends 4×200B (=800 ≤ window).
		const id = Stream.Id.create(0n, Stream.Dir.Uni, true);
		for (let i = 0; i < 4; i++) {
			peer.send({ type: "stream", id, data: new Uint8Array(200), fin: false });
		}
		// A ping after the data acts as a barrier: the Session processes frames in
		// order, so once we see the PONG, all four STREAM frames have been received.
		peer.send({ type: "ping_request", sequence: 1n });
		await waitFor(() => peer.has("ping_response"));

		// All bytes are received but not yet delivered to the app (only the eagerly
		// pulled first chunk, 200B < half the window). Credit-on-receipt would have
		// already emitted MAX_STREAM_DATA here; credit-on-delivery has not.
		expect(peer.count("max_stream_data")).toBe(0);

		// App takes the incoming stream and drains it.
		const reader = session.incomingUnidirectionalStreams.getReader();
		const { value: incoming, done } = await reader.read();
		reader.releaseLock();
		if (done || !incoming) throw new Error("expected an incoming unidirectional stream");

		const streamReader = incoming.getReader();
		let got = 0;
		while (got < 800) {
			const chunk = await streamReader.read();
			if (chunk.done) break;
			got += chunk.value.byteLength;
		}
		expect(got).toBe(800);

		// Delivering the buffered bytes replenishes the window.
		await waitFor(() => peer.count("max_stream_data") > 0);
		session.close();
	});

	test("completed unaccepted streams do not recycle receive credit", async () => {
		// A single-stream/single-payload budget makes any premature replenishment
		// immediately observable. This is the reproduction from issue #299: the peer
		// sends a full stream plus FIN while the application never reads the acceptor.
		const { session, peer } = connect({
			maxStreamsUni: 1n,
			maxData: 100n,
			maxStreamDataUni: 100n,
		});
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		peer.send({
			type: "stream",
			id: Stream.Id.create(0n, Stream.Dir.Uni, true),
			data: new Uint8Array(100),
			fin: true,
		});
		peer.send({ type: "ping_request", sequence: 1n });
		await waitFor(() => peer.has("ping_response"));

		// FIN only completed the wire-level receive. The stream object and all 100
		// bytes are still transport-owned, so neither budget may be recycled yet.
		expect(peer.count("max_streams_uni")).toBe(0);
		expect(peer.count("max_data")).toBe(0);
		expect(peer.count("max_stream_data")).toBe(0);

		const acceptor = session.incomingUnidirectionalStreams.getReader();
		const accepted = await acceptor.read();
		if (accepted.done || !accepted.value) throw new Error("expected an incoming stream");

		// Accepting transfers ownership of the stream object, but the unread payload
		// remains charged to MAX_DATA until the application asks for it.
		await waitFor(() => peer.count("max_streams_uni") === 1);
		expect(peer.count("max_data")).toBe(0);

		const payload = await accepted.value.getReader().read();
		expect(payload.value?.byteLength).toBe(100);
		await waitFor(() => peer.count("max_data") === 1);
		session.close();
	});

	test("accepted active streams do not recycle stream-count credit", async () => {
		const { session, peer } = connect({ maxStreamsUni: 1n });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const id = Stream.Id.create(0n, Stream.Dir.Uni, true);
		peer.send({ type: "stream", id, data: new Uint8Array([1]), fin: false });

		const acceptor = session.incomingUnidirectionalStreams.getReader();
		const accepted = await acceptor.read();
		if (accepted.done || !accepted.value) throw new Error("expected an incoming stream");

		peer.send({ type: "ping_request", sequence: 1n });
		await waitFor(() => peer.has("ping_response"));
		expect(peer.count("max_streams_uni")).toBe(0);

		peer.send({ type: "stream", id, data: new Uint8Array(), fin: true });
		await waitFor(() => peer.count("max_streams_uni") === 1);
		session.close();
	});

	// Regression: a STREAM frame delivered by the read loop after the session has
	// closed used to hit the already-closed incoming-stream controllers, and the
	// throw escaped as an unhandled rejection because #handleStreamFrame was an
	// unawaited async call. These tests capture process-level unhandled rejections
	// so the failure mode is asserted, not just observed in logs.
	describe("STREAM frame vs session teardown", () => {
		const rejections: unknown[] = [];
		const onRejection = (err: unknown) => {
			rejections.push(err);
		};

		beforeEach(async () => {
			process.on("unhandledRejection", onRejection);
			// Drain any late async work from earlier tests before arming the capture,
			// so a stray rejection from another test's teardown can't be misattributed
			// to the test about to run.
			await settle();
			rejections.length = 0;
		});
		afterEach(() => {
			process.off("unhandledRejection", onRejection);
		});

		test("a STREAM frame arriving after close() is dropped without an unhandled rejection", async () => {
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams() });

			session.close({ closeCode: 42, reason: "bye" });

			// Peer-initiated streams still in flight when the session closed: one uni,
			// one bidi — both would hit the now-closed incoming-stream controllers.
			// The third targets a stream id that could only exist pre-close (#recvStreams
			// was cleared by close), so it also takes the new-stream branch.
			peer.send({
				type: "stream",
				id: Stream.Id.create(0n, Stream.Dir.Uni, true),
				data: new Uint8Array(3),
				fin: false,
			});
			peer.send({
				type: "stream",
				id: Stream.Id.create(0n, Stream.Dir.Bi, true),
				data: new Uint8Array(3),
				fin: false,
			});
			peer.send({
				type: "stream",
				id: Stream.Id.create(0n, Stream.Dir.Uni, true),
				data: new Uint8Array(3),
				fin: true,
			});

			await settle();
			expect(rejections).toEqual([]);
			expect(await session.closed).toEqual({ closeCode: 42, reason: "bye" });
		});

		test("a STREAM frame on a send-only stream ID fails the session with 1002", async () => {
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams() });

			// A client-initiated uni stream is send-only from our (client) perspective;
			// the peer writing to it is a protocol violation. Now that #handleStreamFrame
			// is synchronous, the throw propagates to #onData and closes with 1002 —
			// previously it leaked as a rejection while the session limped on.
			peer.send({
				type: "stream",
				id: Stream.Id.create(0n, Stream.Dir.Uni, false),
				data: new Uint8Array([1]),
				fin: false,
			});

			await expectSessionFailure(session);
			await waitFor(() => sentCloseCode(peer) === 1002);
			await settle();
			expect(rejections).toEqual([]);
		});

		test("a stream arriving after the app cancelled the acceptor is refused, not fatal", async () => {
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams() });

			// App stops accepting incoming uni streams while the session stays open.
			await session.incomingUnidirectionalStreams.cancel();
			peer.send({
				type: "stream",
				id: Stream.Id.create(0n, Stream.Dir.Uni, true),
				data: new Uint8Array(3),
				fin: false,
			});

			// Ping barrier: once the pong is seen the STREAM frame above has been fully
			// processed. The session must still be alive — an app-local cancel is not a
			// peer protocol violation.
			peer.send({ type: "ping_request", sequence: 1n });
			await waitFor(() => peer.has("ping_response"), 5000);

			await settle();
			expect(rejections).toEqual([]);
			expect(peer.has("connection_close")).toBe(false);
			session.close();
		});

		test("repeated frames on a refused stream id do not amplify STOP_SENDING or re-credit stream count", async () => {
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams() });

			await session.incomingUnidirectionalStreams.cancel();

			// One peer-initiated uni stream, its payload split across several in-flight
			// STREAM frames (all before the peer could honor any STOP_SENDING). Deleting
			// the stream on refusal would make each later frame re-open it, re-crediting
			// MAX_STREAMS and emitting a STOP_SENDING per frame; keeping it registered
			// means exactly one refusal.
			const id = Stream.Id.create(0n, Stream.Dir.Uni, true);
			for (let i = 0; i < 8; i++) {
				peer.send({ type: "stream", id, data: new Uint8Array(3), fin: false });
			}
			peer.send({ type: "ping_request", sequence: 1n });
			await waitFor(() => peer.has("ping_response"), 5000);

			await settle();
			expect(rejections).toEqual([]);
			// No per-frame amplification: the refused stream never sends STOP_SENDING and
			// never re-credits the peer's stream-count limit.
			expect(peer.count("stop_sending")).toBe(0);
			expect(peer.has("max_streams_uni")).toBe(false);
			expect(peer.has("connection_close")).toBe(false);
			session.close();
		});

		test("refused streams do not leak connection flow-control credit", async () => {
			// Tight connection window (300B). Each refused stream's bytes advance the
			// connection recv offset; if they aren't also counted as consumed, the
			// advertised window never replenishes and the peer is wrongly starved /
			// eventually tripped into a 1002 flow-control close.
			const { session, peer } = connect({ maxData: 300n });
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams() });

			await session.incomingUnidirectionalStreams.cancel();

			// Five distinct refused uni streams carrying 500B total — well past the 300B
			// window. With correct accounting the window keeps pace (MAX_DATA is emitted)
			// and the session stays healthy; the buggy refuse path froze `consumed` and
			// closed with a spurious flow-control error by the 4th stream.
			for (let i = 0n; i < 5n; i++) {
				peer.send({
					type: "stream",
					id: Stream.Id.create(i, Stream.Dir.Uni, true),
					data: new Uint8Array(100),
					fin: false,
				});
			}
			peer.send({ type: "ping_request", sequence: 1n });
			await waitFor(() => peer.has("ping_response"), 5000);

			await settle();
			expect(rejections).toEqual([]);
			expect(peer.has("connection_close")).toBe(false);
			// The connection window advanced, proving refused bytes were credited back.
			expect(peer.has("max_data")).toBe(true);
			session.close();
		});
	});

	describe("close threads its reason into blocked Credit.claim() waiters", () => {
		test("a uni stream blocked on stream-count credit rejects with the graceful close reason", async () => {
			const { session, peer } = connect();
			await session.ready;
			// Peer grants zero uni stream-count credit, so createUnidirectionalStream parks on claim().
			peer.send({ type: "transport_parameters", params: peerParams({ initialMaxStreamsUni: 0n }) });
			peer.send({ type: "ping_request", sequence: 1n });
			await waitFor(() => peer.has("ping_response"));

			const created = settleTo(session.createUnidirectionalStream());
			await flushMicrotasks(); // let the call reach its parked claim()

			session.close({ closeCode: 42, reason: "bye" });

			const err = await created;
			expect(err).toBeInstanceOf(Error);
			expect((err as Error).message).toBe("Connection closed: 42 bye");
		});

		test("a bidi stream blocked on stream-count credit rejects with a CONNECTION_CLOSE frame's reason", async () => {
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams({ initialMaxStreamsBidi: 0n }) });
			peer.send({ type: "ping_request", sequence: 1n });
			await waitFor(() => peer.has("ping_response"));

			const created = settleTo(session.createBidirectionalStream());
			await flushMicrotasks();

			// The peer aborts the connection (CONNECTION_CLOSE / 0x1c); the frame's
			// code/reason must reach the waiter.
			peer.send({ type: "connection_close", code: VarInt.from(7n), reason: "peer gone" });

			const err = await created;
			expect(err).toBeInstanceOf(Error);
			expect((err as Error).message).toBe("Connection closed: 7 peer gone");
		});

		test("a stream write blocked on send credit rejects with the close reason", async () => {
			// Tiny per-stream send window: the first chunk drains it, the next write parks.
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams({ initialMaxStreamDataUni: 5n }) });

			const writable = await session.createUnidirectionalStream();
			const writer = writable.getWriter();
			const wrote = settleTo(writer.write(new Uint8Array(8)));

			// The first 5 bytes flush to the wire, so the write is now parked on the remaining 3.
			await waitFor(() => peer.has("stream"));

			session.close({ closeCode: 9, reason: "credit gone" });

			const err = await wrote;
			expect(err).toBeInstanceOf(Error);
			expect((err as Error).message).toBe("Connection closed: 9 credit gone");
		});

		test("a blocked claim rejects with the reason from a WebSocket-level close", async () => {
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams({ initialMaxStreamsUni: 0n }) });
			peer.send({ type: "ping_request", sequence: 1n });
			await waitFor(() => peer.has("ping_response"));

			const created = settleTo(session.createUnidirectionalStream());
			await flushMicrotasks();

			// The transport closes underneath the session (no CONNECTION_CLOSE frame).
			peer.close({ closeCode: 1001, reason: "going away" });

			const err = await created;
			expect(err).toBeInstanceOf(Error);
			expect((err as Error).message).toBe("Connection closed: 1001 going away");
		});
	});

	test("createBidirectionalStream on an already-closed session rejects with the descriptive reason", async () => {
		const { session } = connect();
		await session.ready;

		// Graceful app-initiated close leaves #closeReason unset, so the synchronous
		// already-closed throw must fall back to #closed's "Connection closed: <code> <reason>",
		// not a generic "Connection closed".
		session.close({ closeCode: 5, reason: "done" });

		const err = await settleTo(session.createBidirectionalStream());
		expect(err).toBeInstanceOf(Error);
		expect((err as Error).message).toBe("Connection closed: 5 done");
	});
});
