import { afterEach, describe, expect, test } from "bun:test";
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
	#closedResolve!: (info: { closeCode?: number; reason?: string }) => void;
	#recv!: ReadableStreamDefaultController<Uint8Array | string>;

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
			write: (chunk) => {
				this.sent.push(chunk);
			},
		});
		this.opened = Promise.resolve({ readable, writable, protocol: "qmux-01", extensions: "" });
		this.closed = new Promise((resolve) => {
			this.#closedResolve = resolve;
		});
	}

	close(info: { closeCode?: number; reason?: string } = {}) {
		this.closeInfo = info;
		this.#closedResolve(info);
	}
	setHighWaterMark() {}

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

/** Drain queued microtasks so a just-issued claim() reaches its parked state.
 *  Parking is pure-microtask work (no I/O), so a bounded drain is deterministic. */
async function flushMicrotasks(turns = 10): Promise<void> {
	for (let i = 0; i < turns; i++) await Promise.resolve();
}

/** Settle a promise to its value or its rejection error, without leaking an
 *  unhandled rejection while the test arranges the teardown that resolves it. */
function settle<T>(p: Promise<T>): Promise<T | Error> {
	return p.then(
		(v) => v,
		(e) => e,
	);
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

	test("close() sends CONNECTION_CLOSE, resolves closed, and is idempotent", async () => {
		const { session, peer } = connect();
		await session.ready;

		session.close({ closeCode: 42, reason: "bye" });
		expect(await session.closed).toEqual({ closeCode: 42, reason: "bye" });
		await waitFor(() => peer.has("connection_close"));

		// Second close is a no-op: the resolved info is unchanged.
		session.close({ closeCode: 7, reason: "again" });
		expect(await session.closed).toEqual({ closeCode: 42, reason: "bye" });
	});

	test("a text frame closes the session with a protocol error", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.sendText("not binary");
		const info = await session.closed;
		expect(info.closeCode).toBe(1003);
	});

	test("an unnegotiated RESET_STREAM_AT closes the session", async () => {
		// The peer negotiates qmux-01, which never advertises `reset_stream_at`, so
		// a RESET_STREAM_AT (0x24) frame is a protocol violation. Hand-built bytes:
		// type=0x24, id=3, code=0, final_size=0, reliable_size=0.
		const { session, peer } = connect();
		await session.ready;
		peer.sendRaw(new Uint8Array([0x24, 0x03, 0x00, 0x00, 0x00]));
		const info = await session.closed;
		expect(info.closeCode).toBe(1002);
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
		expect(await session.closed).toEqual({ closeCode: 1002, reason: "Protocol violation" });
	});

	test("datagrams: a DATAGRAM we never advertised support for is fatal", async () => {
		// ourParams.maxDatagramFrameSize = 0 → we advertised no datagram support, so
		// receiving one at all is a PROTOCOL_VIOLATION (RFC 9221), not a droppable frame.
		const { session, peer } = connect({ maxDatagramFrameSize: 0n });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		peer.send({ type: "datagram", data: new Uint8Array([1, 2, 3]) });
		expect(await session.closed).toEqual({ closeCode: 1002, reason: "Protocol violation" });
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

	describe("close threads its reason into blocked Credit.claim() waiters", () => {
		test("a uni stream blocked on stream-count credit rejects with the graceful close reason", async () => {
			const { session, peer } = connect();
			await session.ready;
			// Peer grants zero uni stream-count credit, so createUnidirectionalStream parks on claim().
			peer.send({ type: "transport_parameters", params: peerParams({ initialMaxStreamsUni: 0n }) });
			peer.send({ type: "ping_request", sequence: 1n });
			await waitFor(() => peer.has("ping_response"));

			const created = settle(session.createUnidirectionalStream());
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

			const created = settle(session.createBidirectionalStream());
			await flushMicrotasks();

			// The peer aborts the connection; the frame's code/reason must reach the waiter.
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
			const wrote = settle(writer.write(new Uint8Array(8)));

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

			const created = settle(session.createUnidirectionalStream());
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

		const err = await settle(session.createBidirectionalStream());
		expect(err).toBeInstanceOf(Error);
		expect((err as Error).message).toBe("Connection closed: 5 done");
	});
});
