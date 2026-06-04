import { afterEach, describe, expect, test } from "bun:test";
import * as Frame from "./frame.ts";
import { DEFAULT_MAX_RECORD_SIZE, type TransportParams } from "./frame.ts";
import Session, { type Config } from "./session.ts";
import * as Stream from "./stream.ts";

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

function peerParams(overrides: Partial<TransportParams> = {}): TransportParams {
	return {
		maxIdleTimeout: 0n,
		initialMaxData: 1_000_000n,
		initialMaxStreamDataBidiLocal: 100_000n,
		initialMaxStreamDataBidiRemote: 100_000n,
		initialMaxStreamDataUni: 100_000n,
		initialMaxStreamsBidi: 100n,
		initialMaxStreamsUni: 100n,
		maxRecordSize: DEFAULT_MAX_RECORD_SIZE,
		...overrides,
	};
}

function connect(config?: Config): { session: Session; peer: FakePeer } {
	(globalThis as { WebSocketStream?: unknown }).WebSocketStream = FakePeer;
	// Idle timer off so a stray interval can't interfere with the test.
	const session = new Session("https://example/test", {
		withoutProtocol: true,
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
});
