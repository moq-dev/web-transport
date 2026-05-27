import { Credit } from "./credit.ts";
import type { TransportParams, Version } from "./frame.ts";
import * as Frame from "./frame.ts";
import { DEFAULT_TRANSPORT_PARAMS, isQmux, MAX_FRAME_PAYLOAD } from "./frame.ts";
import * as Stream from "./stream.ts";
import { VarInt } from "./varint.ts";

/** Configuration for a QMux session. */
export interface Config {
	/** Max concurrent bidirectional streams the peer can open. */
	maxStreamsBidi?: bigint;
	/** Max concurrent unidirectional streams the peer can open. */
	maxStreamsUni?: bigint;
	/** Connection-level receive window in bytes. */
	maxData?: bigint;
	/** Per-stream receive window for bidi streams we initiate. */
	maxStreamDataBidiLocal?: bigint;
	/** Per-stream receive window for bidi streams the peer initiates. */
	maxStreamDataBidiRemote?: bigint;
	/** Per-stream receive window for uni streams. */
	maxStreamDataUni?: bigint;
	/** Idle timeout in milliseconds (0 = disabled). */
	maxIdleTimeout?: bigint;
	/** Maximum QMux Record size in bytes (draft-01). */
	maxRecordSize?: bigint;
}

const DEFAULT_CONFIG: Required<Config> = {
	maxStreamsBidi: 100n,
	maxStreamsUni: 100n,
	maxData: 1_048_576n,
	maxStreamDataBidiLocal: 262_144n,
	maxStreamDataBidiRemote: 262_144n,
	maxStreamDataUni: 262_144n,
	maxIdleTimeout: 30_000n,
	maxRecordSize: Frame.DEFAULT_MAX_RECORD_SIZE,
};

function configToTransportParams(config: Required<Config>): TransportParams {
	return {
		maxIdleTimeout: config.maxIdleTimeout,
		initialMaxData: config.maxData,
		initialMaxStreamDataBidiLocal: config.maxStreamDataBidiLocal,
		initialMaxStreamDataBidiRemote: config.maxStreamDataBidiRemote,
		initialMaxStreamDataUni: config.maxStreamDataUni,
		initialMaxStreamsBidi: config.maxStreamsBidi,
		initialMaxStreamsUni: config.maxStreamsUni,
		maxRecordSize: config.maxRecordSize,
	};
}

// TODO Implement this
export class Datagrams implements WebTransportDatagramDuplexStream {
	incomingHighWaterMark: number;
	incomingMaxAge: number | null;
	readonly maxDatagramSize: number;
	outgoingHighWaterMark: number;
	outgoingMaxAge: number | null;
	readonly readable: ReadableStream;
	readonly writable: WritableStream;

	constructor() {
		this.incomingHighWaterMark = 1024;
		this.incomingMaxAge = null;
		this.maxDatagramSize = 1200;
		this.outgoingHighWaterMark = 1024;
		this.outgoingMaxAge = null;
		this.readable = new ReadableStream<Uint8Array>({});
		this.writable = new WritableStream<Uint8Array>({});
	}
}

/** Options for the WebTransport-over-WebSocket polyfill. */
export interface SessionOptions extends WebTransportOptions {
	/** The QMux wire-format version to use.
	 *
	 * The version is fixed at construction. Only the corresponding bare ALPN and
	 * `{prefix}{proto}` subprotocols are advertised during the WebSocket handshake.
	 * Callers that want to negotiate across multiple QMux drafts should attempt
	 * separate `Session` instances per version; this class does not cross-product
	 * versions on its own.
	 */
	version: Version;

	/** Application-level subprotocols to request during the WebSocket handshake.
	 *
	 * Each protocol is offered as `{version.prefix}{proto}` (e.g. `qmux-01.moq-03`).
	 * The bare version ALPN is also advertised as a fallback.
	 */
	protocols?: string[];

	/** QMux flow control configuration. Only used for the QMux wire formats. */
	config?: Config;
}

/** Get the ALPN/subprotocol prefix for a version. */
function versionPrefix(version: Version): string {
	switch (version) {
		case "qmux-01":
			return "qmux-01.";
		case "qmux-00":
			return "qmux-00.";
		case "webtransport":
			return "webtransport.";
	}
}

/** Strip the negotiated subprotocol's expected prefix to recover the app protocol.
 *
 * The QMux version is already known (caller-supplied at construction); this only
 * peels off the prefix. Accepts the bare version ALPN (returns "") and the
 * prefixed form `{version.prefix}{proto}`. Unknown values yield "".
 */
function parseProtocol(raw: string, version: Version): string {
	if (raw === "" || raw === version) return "";
	const prefix = versionPrefix(version);
	return raw.startsWith(prefix) ? raw.slice(prefix.length) : "";
}

/** Per-stream flow control state. */
interface StreamFlowState {
	sendCredit: Credit;
	recvMax: bigint;
	recvOffset: bigint;
	recvConsumed: bigint;
}

export default class Session implements WebTransport {
	#ws: WebSocket;
	#isServer = false;
	#closed?: Error;
	#closeReason?: Error;

	#sendStreams = new Map<bigint, WritableStreamDefaultController>();
	#recvStreams = new Map<bigint, ReadableStreamDefaultController<Uint8Array>>();

	#nextUniStreamId = 0n;
	#nextBiStreamId = 0n;

	readonly #version: Version;

	/** The negotiated application-level subprotocol, or empty string if none.
	 *
	 * The prefix is stripped; this returns only the application protocol name.
	 */
	#protocol = "";
	get protocol(): string {
		return this.#protocol;
	}

	readonly ready: Promise<void>;
	#readyResolve: () => void;
	readonly closed: Promise<WebTransportCloseInfo>;
	#closedResolve: (info: WebTransportCloseInfo) => void;

	readonly incomingBidirectionalStreams: ReadableStream<WebTransportBidirectionalStream>;
	#incomingBidirectionalStreams!: ReadableStreamDefaultController<WebTransportBidirectionalStream>;
	readonly incomingUnidirectionalStreams: ReadableStream<ReadableStream<Uint8Array>>;
	#incomingUnidirectionalStreams!: ReadableStreamDefaultController<ReadableStream<Uint8Array>>;

	// TODO: Implement datagrams
	readonly datagrams = new Datagrams();

	// Flow control state
	#config: Required<Config>;
	#ourParams: TransportParams;
	#peerParams: TransportParams = { ...DEFAULT_TRANSPORT_PARAMS };
	#paramsReceived = false;

	// Connection-level send credit
	#connCredit: Credit;

	// Connection-level recv flow control
	#recvDataOffset = 0n;
	#recvDataMax = 0n;
	#recvDataConsumed = 0n;

	// Per-stream flow control
	#streamFlow = new Map<bigint, StreamFlowState>();

	// Stream count tracking via Credit (for sending — peer's limits)
	#bidiStreamCredit: Credit;
	#uniStreamCredit: Credit;

	// Stream count tracking via Credit (for receiving — our limits)
	#recvBiCredit: Credit;
	#recvUniCredit: Credit;

	// QMux01 idle-timeout tracking (engaged once we've received the peer's params).
	#lastRecvAt = Date.now();
	#lastSendAt = Date.now();
	#nextPingSeq = 0;
	#idleTimer?: ReturnType<typeof setInterval>;

	constructor(url: string | URL, options: SessionOptions) {
		if (options.requireUnreliable) {
			throw new Error("not allowed to use WebSocket; requireUnreliable is true");
		}

		if (options.serverCertificateHashes) {
			console.warn("serverCertificateHashes is not supported; trying anyway");
		}

		url = Session.#convertToWebSocketUrl(url);

		// Merge user config with defaults
		this.#config = { ...DEFAULT_CONFIG, ...options.config };
		this.#ourParams = configToTransportParams(this.#config);

		// The version is pinned at construction. Build the subprotocol list with
		// only this version's bare ALPN + prefixed application protocols — no
		// cross-product. Callers that need fallback across versions instantiate
		// separate Sessions per version themselves.
		this.#version = options.version;
		const prefix = versionPrefix(this.#version);
		const subprotocols: string[] = [this.#version];
		for (const p of options.protocols ?? []) {
			subprotocols.push(`${prefix}${p}`);
		}
		this.#ws = new WebSocket(url, subprotocols);

		// Initialize credits up front — version is known immediately.
		if (isQmux(this.#version)) {
			this.#connCredit = new Credit(0n);
			this.#bidiStreamCredit = new Credit(0n);
			this.#uniStreamCredit = new Credit(0n);
		} else {
			// No flow control for WebTransport — set unlimited.
			this.#connCredit = new Credit(BigInt(Number.MAX_SAFE_INTEGER));
			this.#bidiStreamCredit = new Credit(BigInt(Number.MAX_SAFE_INTEGER));
			this.#uniStreamCredit = new Credit(BigInt(Number.MAX_SAFE_INTEGER));
		}
		this.#recvBiCredit = new Credit(this.#config.maxStreamsBidi);
		this.#recvUniCredit = new Credit(this.#config.maxStreamsUni);

		const ready = Promise.withResolvers<void>();
		this.ready = ready.promise;
		this.#readyResolve = ready.resolve;

		const closed = Promise.withResolvers<WebTransportCloseInfo>();
		this.closed = closed.promise;
		this.#closedResolve = closed.resolve;

		this.#ws.binaryType = "arraybuffer";
		this.#ws.onopen = () => {
			// Recover the application protocol from the negotiated subprotocol.
			// The QMux version is fixed; only the app protocol comes off the wire.
			this.#protocol = parseProtocol(this.#ws.protocol, this.#version);

			if (isQmux(this.#version)) {
				this.#recvDataMax = this.#ourParams.initialMaxData;
				this.#sendTransportParameters();
			}

			this.#readyResolve();
		};
		this.#ws.onmessage = (event) => this.#handleMessage(event);
		this.#ws.onerror = (event) => this.#handleError(event);
		this.#ws.onclose = (event) => this.#handleClose(event);

		this.incomingBidirectionalStreams = new ReadableStream<WebTransportBidirectionalStream>({
			start: (controller) => {
				this.#incomingBidirectionalStreams = controller;
			},
		});

		this.incomingUnidirectionalStreams = new ReadableStream<ReadableStream<Uint8Array>>({
			start: (controller) => {
				this.#incomingUnidirectionalStreams = controller;
			},
		});

		if (!this.#incomingBidirectionalStreams || !this.#incomingUnidirectionalStreams) {
			throw new Error("ReadableStream didn't call start");
		}
	}

	static #convertToWebSocketUrl(url: string | URL): string {
		const urlObj = typeof url === "string" ? new URL(url) : url;

		// Convert https:// to wss:// and http:// to ws://
		let protocol = urlObj.protocol;
		if (protocol === "https:") {
			protocol = "wss:";
		} else if (protocol === "http:") {
			protocol = "ws:";
		} else if (protocol !== "ws:" && protocol !== "wss:") {
			throw new Error(`Unsupported protocol: ${protocol}`);
		}

		// Build WebSocket URL
		return `${protocol}//${urlObj.host}${urlObj.pathname}${urlObj.search}`;
	}

	#handleMessage(event: MessageEvent) {
		if (!(event.data instanceof ArrayBuffer)) return;

		const data = new Uint8Array(event.data);
		this.#lastRecvAt = Date.now();
		try {
			if (this.#version === "qmux-01") {
				// QMux01: each WS message is a record containing one or more frames
				const frames = Frame.decodeRecord(data);
				for (const frame of frames) {
					this.#recvFrame(frame);
				}
			} else {
				const frame = Frame.decode(data, this.#version);
				if (frame !== null) {
					this.#recvFrame(frame);
				}
			}
		} catch (error) {
			console.error("Failed to decode frame:", error);
			this.close({ closeCode: 1002, reason: "Protocol violation" });
		}
	}

	#handleError(event: Event) {
		if (this.#closed) return;

		this.#closed = new Error(`WebSocket error: ${event.type}`);
		this.#close(1006, "WebSocket error");
	}

	#handleClose(event: CloseEvent) {
		if (this.#closed) return;

		this.#closed = new Error(`Connection closed: ${event.code} ${event.reason}`);
		this.#close(event.code, event.reason);
	}

	#recvFrame(frame: Frame.Any) {
		if (frame.type === "stream") {
			this.#handleStreamFrame(frame);
		} else if (frame.type === "reset_stream") {
			this.#handleResetStream(frame);
		} else if (frame.type === "stop_sending") {
			this.#handleStopSending(frame);
		} else if (frame.type === "connection_close") {
			this.#closeReason = new Error(`Connection closed: ${frame.code.value}: ${frame.reason}`);
			this.#ws.close();
		} else if (frame.type === "transport_parameters") {
			this.#handleTransportParameters(frame.params);
		} else if (frame.type === "max_data") {
			this.#connCredit.increaseMax(frame.max);
		} else if (frame.type === "max_stream_data") {
			const flow = this.#streamFlow.get(frame.id.value.value);
			if (flow) flow.sendCredit.increaseMax(frame.max);
		} else if (frame.type === "max_streams_bidi") {
			this.#bidiStreamCredit.increaseMax(frame.max);
		} else if (frame.type === "max_streams_uni") {
			this.#uniStreamCredit.increaseMax(frame.max);
		} else if (frame.type === "ping_request") {
			// Respond to ping requests
			this.#sendPriorityFrame({ type: "ping_response", sequence: frame.sequence });
		} else if (frame.type === "ping_response") {
			// Ping response received, no action needed
		} else if (
			frame.type === "data_blocked" ||
			frame.type === "stream_data_blocked" ||
			frame.type === "streams_blocked_bidi" ||
			frame.type === "streams_blocked_uni"
		) {
			// Informational, no action needed
		}
	}

	#handleTransportParameters(params: TransportParams) {
		if (this.#paramsReceived) return;
		this.#paramsReceived = true;
		this.#peerParams = params;

		this.#connCredit.increaseMax(params.initialMaxData);
		this.#bidiStreamCredit.increaseMax(params.initialMaxStreamsBidi);
		this.#uniStreamCredit.increaseMax(params.initialMaxStreamsUni);

		// Update per-stream send credits for locally-opened streams created before params arrived.
		// Peer-opened streams can't exist yet (params are the first frame on the wire).
		for (const [streamIdVal, flow] of this.#streamFlow) {
			const id = new Stream.Id(VarInt.from(streamIdVal));
			const sendLimit =
				id.dir === Stream.Dir.Bi ? params.initialMaxStreamDataBidiRemote : params.initialMaxStreamDataUni;
			flow.sendCredit.increaseMax(sendLimit);
		}

		this.#startIdleTimerIfEnabled();
	}

	/** Effective idle timeout in ms, or 0 if disabled.
	 *
	 * Per RFC 9000 §10.1, the effective value is `min(our, peer)` of the non-zero advertised values
	 * (or the single non-zero one). If both are zero, idle timeouts are disabled.
	 */
	#effectiveIdleTimeoutMs(): bigint {
		if (this.#version !== "qmux-01") return 0n;
		const a = this.#ourParams.maxIdleTimeout;
		const b = this.#peerParams.maxIdleTimeout;
		if (a === 0n && b === 0n) return 0n;
		if (a === 0n) return b;
		if (b === 0n) return a;
		return a < b ? a : b;
	}

	#startIdleTimerIfEnabled() {
		const timeoutMs = this.#effectiveIdleTimeoutMs();
		if (timeoutMs === 0n) return;
		// Poll at a fraction of the timeout — frequent enough to trigger pings on time
		// but not so frequent it burns CPU on otherwise-quiet sessions.
		const tickMs = Math.max(50, Number(timeoutMs) / 6);
		this.#idleTimer = setInterval(() => this.#idleTick(Number(timeoutMs)), tickMs);
	}

	#idleTick(timeoutMs: number) {
		if (this.#closed) {
			if (this.#idleTimer) clearInterval(this.#idleTimer);
			return;
		}
		const now = Date.now();
		if (now - this.#lastRecvAt > timeoutMs) {
			// Peer has gone silent past the negotiated limit.
			this.#closeReason = new Error("idle timeout");
			this.#ws.close();
			if (this.#idleTimer) clearInterval(this.#idleTimer);
			return;
		}
		// Keep-alive: nudge the peer when our outbound side has been silent for a third
		// of the timeout. Any frame counts as activity, so this only fires when truly idle.
		if (now - this.#lastSendAt > timeoutMs / 3) {
			const seq = this.#nextPingSeq;
			this.#nextPingSeq = (this.#nextPingSeq + 1) >>> 0;
			try {
				this.#sendPriorityFrame({ type: "ping_request", sequence: BigInt(seq) });
			} catch {
				// Best effort — if the send fails, the close path will fire shortly.
			}
		}
	}

	async #claimSendCredit(streamId: bigint, desired: bigint): Promise<bigint> {
		const flow = this.#streamFlow.get(streamId);
		if (!flow) return desired;

		while (true) {
			// 1. Try stream credit
			const streamClaimed = flow.sendCredit.tryClaim(desired);
			if (streamClaimed === 0n) {
				if (this.#closed) throw this.#closeReason || new Error("Connection closed");
				// Wait for stream credit, then release and retry to coordinate with conn credit
				const claimed = await flow.sendCredit.claim(desired);
				flow.sendCredit.release(claimed);
				continue;
			}

			// 2. Try connection credit
			const connClaimed = this.#connCredit.tryClaim(streamClaimed);
			if (connClaimed === 0n) {
				flow.sendCredit.release(streamClaimed);
				if (this.#closed) throw this.#closeReason || new Error("Connection closed");
				const claimed = await this.#connCredit.claim(1n);
				this.#connCredit.release(claimed);
				continue;
			}

			// Return excess stream credit if connection had less
			if (connClaimed < streamClaimed) {
				flow.sendCredit.release(streamClaimed - connClaimed);
			}

			return connClaimed;
		}
	}

	#accountRecv(streamId: bigint, bytes: number): boolean {
		if (!isQmux(this.#version) || bytes === 0) return true;

		const bytesN = BigInt(bytes);

		// Connection-level check
		if (this.#recvDataOffset + bytesN > this.#recvDataMax) {
			return false;
		}
		this.#recvDataOffset += bytesN;

		// Stream-level check
		const flow = this.#streamFlow.get(streamId);
		if (flow) {
			if (flow.recvOffset + bytesN > flow.recvMax) {
				return false;
			}
			flow.recvOffset += bytesN;
		}

		return true;
	}

	#accountConsumed(streamId: bigint, bytes: number) {
		if (!isQmux(this.#version) || bytes === 0) return;

		// Track connection-level consumed (stable, not reset by per-stream updates)
		this.#recvDataConsumed += BigInt(bytes);

		const flow = this.#streamFlow.get(streamId);
		if (flow) {
			flow.recvConsumed += BigInt(bytes);
			this.#maybeSendMaxStreamData(streamId, flow);
		}
		this.#maybeSendMaxData();
	}

	#maybeSendMaxData() {
		const window = this.#ourParams.initialMaxData;
		if (window === 0n) return;

		const threshold = window / 2n;
		if (this.#recvDataConsumed >= threshold) {
			const newMax = this.#recvDataOffset + window;
			if (newMax > this.#recvDataMax) {
				this.#recvDataMax = newMax;
				this.#recvDataConsumed = 0n;
				this.#sendPriorityFrame({ type: "max_data", max: newMax });
			}
		}
	}

	#maybeSendMaxStreamData(streamId: bigint, flow: StreamFlowState) {
		const id = new Stream.Id(VarInt.from(streamId));

		let initialWindow: bigint;
		if (id.dir === Stream.Dir.Bi) {
			// Check if we initiated this stream
			initialWindow =
				id.serverInitiated === this.#isServer
					? this.#ourParams.initialMaxStreamDataBidiLocal
					: this.#ourParams.initialMaxStreamDataBidiRemote;
		} else {
			initialWindow = this.#ourParams.initialMaxStreamDataUni;
		}

		if (initialWindow === 0n) return;

		const threshold = initialWindow / 2n;
		if (flow.recvConsumed >= threshold) {
			const newMax = flow.recvOffset + initialWindow;
			if (newMax > flow.recvMax) {
				flow.recvMax = newMax;
				flow.recvConsumed = 0n;
				this.#sendPriorityFrame({ type: "max_stream_data", id, max: newMax });
			}
		}
	}

	/** Replenish stream count credit for a peer-initiated stream and send MAX_STREAMS if needed. */
	#replenishStreamCredit(dir: Stream.DirType) {
		if (!isQmux(this.#version)) return;

		const credit = dir === Stream.Dir.Bi ? this.#recvBiCredit : this.#recvUniCredit;
		const newMax = credit.consume(1n);
		if (newMax !== null) {
			if (dir === Stream.Dir.Bi) {
				this.#sendPriorityFrame({ type: "max_streams_bidi", max: newMax });
			} else {
				this.#sendPriorityFrame({ type: "max_streams_uni", max: newMax });
			}
		}
	}

	/** Delete stream flow state only when both send and recv sides are gone. */
	#maybeDeleteStreamFlow(streamId: bigint) {
		if (!this.#sendStreams.has(streamId) && !this.#recvStreams.has(streamId)) {
			const flow = this.#streamFlow.get(streamId);
			if (flow) {
				flow.sendCredit.close();
				this.#streamFlow.delete(streamId);
			}
		}
	}

	async #handleStreamFrame(frame: Frame.Data) {
		if (frame.data.byteLength > MAX_FRAME_PAYLOAD) {
			this.close({ closeCode: 1002, reason: "frame too large" });
			return;
		}

		const streamId = frame.id.value.value;

		if (!frame.id.canRecv(this.#isServer)) {
			throw new Error("Invalid stream ID direction");
		}

		let stream = this.#recvStreams.get(streamId);
		if (!stream) {
			// We created the stream, we can skip it.
			if (frame.id.serverInitiated === this.#isServer) {
				return;
			}
			if (!frame.id.canRecv(this.#isServer)) {
				throw new Error("received write-only stream");
			}

			// Validate stream count limits (QMux only)
			// Per QUIC RFC 9000 §4.6, the limit applies to the stream index.
			// A peer opening stream index N implicitly opens all streams 0..N.
			if (isQmux(this.#version)) {
				const credit = frame.id.dir === Stream.Dir.Bi ? this.#recvBiCredit : this.#recvUniCredit;
				if (!credit.receiveUpTo(frame.id.index + 1n)) {
					this.close({ closeCode: 1002, reason: "stream limit exceeded" });
					return;
				}
			}

			// Initialize flow control state for new stream
			if (isQmux(this.#version)) {
				const recvMax =
					frame.id.dir === Stream.Dir.Bi
						? this.#ourParams.initialMaxStreamDataBidiRemote
						: this.#ourParams.initialMaxStreamDataUni;

				// For send side on bidi: peer's bidi_local is our send limit
				const sendMax = frame.id.dir === Stream.Dir.Bi ? this.#peerParams.initialMaxStreamDataBidiLocal : 0n;

				this.#streamFlow.set(streamId, {
					sendCredit: new Credit(sendMax),
					recvMax,
					recvOffset: 0n,
					recvConsumed: 0n,
				});
			}

			// Validate recv flow control before accepting
			if (!this.#accountRecv(streamId, frame.data.byteLength)) {
				this.close({ closeCode: 1002, reason: "flow control error" });
				return;
			}

			const reader = new ReadableStream<Uint8Array>({
				start: (controller) => {
					stream = controller;
					this.#recvStreams.set(streamId, controller);
				},
				cancel: () => {
					this.#sendPriorityFrame({
						type: "stop_sending",
						id: frame.id,
						code: VarInt.from(0),
					});

					this.#recvStreams.delete(streamId);
					this.#replenishStreamCredit(frame.id.dir);
					this.#maybeDeleteStreamFlow(streamId);
				},
			});

			if (!stream) {
				throw new Error("ReadableStream didn't call start");
			}

			if (frame.id.dir === Stream.Dir.Bi) {
				// Incoming bidirectional stream
				const writer = new WritableStream<Uint8Array>({
					start: (controller) => {
						this.#sendStreams.set(streamId, controller);
					},
					write: async (chunk) => {
						await Promise.race([this.#sendStreamData(frame.id, chunk), this.closed]);
					},
					abort: (e) => {
						console.warn("abort", e);
						this.#sendPriorityFrame({
							type: "reset_stream",
							id: frame.id,
							code: VarInt.from(0),
						});

						this.#sendStreams.delete(streamId);
						this.#maybeDeleteStreamFlow(streamId);
					},
					close: async () => {
						await Promise.race([
							this.#sendFrame({
								type: "stream",
								id: frame.id,
								data: new Uint8Array(),
								fin: true,
							}),
							this.closed,
						]);

						this.#sendStreams.delete(streamId);
						this.#maybeDeleteStreamFlow(streamId);
					},
				});

				this.#incomingBidirectionalStreams.enqueue({ readable: reader, writable: writer });
			} else {
				this.#incomingUnidirectionalStreams.enqueue(reader);
			}
		} else {
			// Existing stream — validate recv flow control
			if (!this.#accountRecv(streamId, frame.data.byteLength)) {
				this.close({ closeCode: 1002, reason: "flow control error" });
				return;
			}
		}

		if (frame.data.byteLength > 0) {
			stream.enqueue(frame.data);
			// Account consumed when data is enqueued to the reader
			this.#accountConsumed(streamId, frame.data.byteLength);
		}

		if (frame.fin) {
			stream.close();
			this.#recvStreams.delete(streamId);
			if (frame.id.serverInitiated !== this.#isServer) {
				this.#replenishStreamCredit(frame.id.dir);
			}
			this.#maybeDeleteStreamFlow(streamId);
		}
	}

	#handleResetStream(frame: Frame.ResetStream) {
		const streamId = frame.id.value.value;
		const stream = this.#recvStreams.get(streamId);
		if (!stream) return;

		stream.error(new Error(`RESET_STREAM: ${frame.code.value}`));
		this.#recvStreams.delete(streamId);
		if (frame.id.serverInitiated !== this.#isServer) {
			this.#replenishStreamCredit(frame.id.dir);
		}
		this.#maybeDeleteStreamFlow(streamId);
	}

	#handleStopSending(frame: Frame.StopSending) {
		const streamId = frame.id.value.value;
		const stream = this.#sendStreams.get(streamId);
		if (!stream) return;

		stream.error(new Error(`STOP_SENDING: ${frame.code.value}`));
		this.#sendStreams.delete(streamId);

		this.#sendPriorityFrame({
			type: "reset_stream",
			id: frame.id,
			code: frame.code,
		});

		this.#maybeDeleteStreamFlow(streamId);
	}

	#sendTransportParameters() {
		const frame: Frame.TransportParameters = {
			type: "transport_parameters",
			params: this.#ourParams,
		};
		// QMux01 over WebSocket uses the WS message boundary as the implicit record
		// boundary; no extra size prefix is required.
		this.#sendBytes(Frame.encode(frame, this.#version));
	}

	/** Send raw frame bytes, validating against the peer's max_record_size for QMux01. */
	#sendBytes(bytes: Uint8Array) {
		if (this.#version === "qmux-01") {
			// Before the peer's TRANSPORT_PARAMETERS arrive, use the draft-01 default
			// (16382) so we don't accidentally send something the peer will reject.
			const limit = this.#paramsReceived ? this.#peerParams.maxRecordSize : Frame.DEFAULT_MAX_RECORD_SIZE;
			if (BigInt(bytes.byteLength) > limit) {
				throw new Error(`record exceeds peer max_record_size (${bytes.byteLength} > ${limit})`);
			}
		}
		this.#ws.send(bytes);
		this.#lastSendAt = Date.now();
	}

	async #sendStreamDataWithFlowControl(id: Stream.Id, streamId: bigint, data: Uint8Array) {
		for (let offset = 0; offset < data.byteLength; ) {
			const remaining = data.byteLength - offset;
			// Cap by both the static frame-payload ceiling and the peer's record limit
			// (qmux-01 only — once params are received). Leave 32 bytes of headroom for
			// the STREAM frame header (frame type + stream id + length varints).
			let chunkMax = Math.min(remaining, MAX_FRAME_PAYLOAD);
			if (this.#version === "qmux-01" && this.#paramsReceived) {
				const peerLimit = Number(this.#peerParams.maxRecordSize) - 32;
				if (peerLimit > 0) {
					chunkMax = Math.min(chunkMax, peerLimit);
				}
			}

			// Claim flow control credit (stream + connection)
			const allowed = await this.#claimSendCredit(streamId, BigInt(chunkMax));
			const sendable = Number(allowed);

			const chunk = data.subarray(offset, offset + sendable);

			try {
				await this.#sendFrame({
					type: "stream",
					id,
					data: chunk,
					fin: false,
				});
			} catch (e) {
				// Return claimed credits on send failure
				if (sendable > 0) {
					const flow = this.#streamFlow.get(streamId);
					if (flow) flow.sendCredit.release(BigInt(sendable));
					this.#connCredit.release(BigInt(sendable));
				}
				throw e;
			}

			offset += sendable;
		}
	}

	async #sendStreamData(id: Stream.Id, data: Uint8Array) {
		const streamId = id.value.value;
		if (isQmux(this.#version)) {
			await this.#sendStreamDataWithFlowControl(id, streamId, data);
		} else {
			for (let offset = 0; offset < data.byteLength; offset += MAX_FRAME_PAYLOAD) {
				const end = Math.min(offset + MAX_FRAME_PAYLOAD, data.byteLength);
				const chunk = data.subarray(offset, end);
				await this.#sendFrame({
					type: "stream",
					id,
					data: chunk,
					fin: false,
				});
			}
		}
	}

	async #sendFrame(frame: Frame.Any) {
		// Add some backpressure so we don't saturate the connection
		while (this.#ws.bufferedAmount > 64 * 1024) {
			await new Promise((resolve) => setTimeout(resolve, 10));
		}

		this.#sendBytes(Frame.encode(frame, this.#version));
	}

	#sendPriorityFrame(frame: Frame.Any) {
		this.#sendBytes(Frame.encode(frame, this.#version));
	}

	async createBidirectionalStream(): Promise<WebTransportBidirectionalStream> {
		await this.ready;

		if (this.#closed) {
			throw this.#closeReason || new Error("Connection closed");
		}

		// Wait for stream count permit
		await this.#bidiStreamCredit.claim(1n);

		const streamId = Stream.Id.create(this.#nextBiStreamId++, Stream.Dir.Bi, this.#isServer);
		const streamIdVal = streamId.value.value;

		// Initialize flow control for this stream
		if (isQmux(this.#version)) {
			this.#streamFlow.set(streamIdVal, {
				sendCredit: new Credit(this.#peerParams.initialMaxStreamDataBidiRemote),
				recvMax: this.#ourParams.initialMaxStreamDataBidiLocal,
				recvOffset: 0n,
				recvConsumed: 0n,
			});
		}

		const writer = new WritableStream<Uint8Array>({
			start: (controller) => {
				this.#sendStreams.set(streamIdVal, controller);
			},
			write: async (chunk) => {
				await Promise.race([this.#sendStreamData(streamId, chunk), this.closed]);
			},
			abort: (e) => {
				console.warn("abort", e);
				this.#sendPriorityFrame({
					type: "reset_stream",
					id: streamId,
					code: VarInt.from(0),
				});

				this.#sendStreams.delete(streamIdVal);
				this.#maybeDeleteStreamFlow(streamIdVal);
			},
			close: async () => {
				await Promise.race([
					this.#sendFrame({
						type: "stream",
						id: streamId,
						data: new Uint8Array(),
						fin: true,
					}),
					this.closed,
				]);

				this.#sendStreams.delete(streamIdVal);
				this.#maybeDeleteStreamFlow(streamIdVal);
			},
		});

		const reader = new ReadableStream<Uint8Array>({
			start: (controller) => {
				this.#recvStreams.set(streamIdVal, controller);
			},
			cancel: async () => {
				this.#sendPriorityFrame({
					type: "stop_sending",
					id: streamId,
					code: VarInt.from(0),
				});

				this.#recvStreams.delete(streamIdVal);
				this.#maybeDeleteStreamFlow(streamIdVal);
			},
		});

		return { readable: reader, writable: writer };
	}

	async createUnidirectionalStream(): Promise<WritableStream<Uint8Array>> {
		await this.ready;

		if (this.#closed) {
			throw this.#closed;
		}

		// Wait for stream count permit
		await this.#uniStreamCredit.claim(1n);

		const streamId = Stream.Id.create(this.#nextUniStreamId++, Stream.Dir.Uni, this.#isServer);
		const streamIdVal = streamId.value.value;

		// Initialize flow control for this stream
		if (isQmux(this.#version)) {
			this.#streamFlow.set(streamIdVal, {
				sendCredit: new Credit(this.#peerParams.initialMaxStreamDataUni),
				recvMax: 0n,
				recvOffset: 0n,
				recvConsumed: 0n,
			});
		}

		const session = this;

		const writer = new WritableStream<Uint8Array>({
			start: (controller) => {
				session.#sendStreams.set(streamIdVal, controller);
			},
			async write(chunk) {
				await Promise.race([session.#sendStreamData(streamId, chunk), session.closed]);
			},
			abort(e) {
				console.warn("abort", e);
				session.#sendPriorityFrame({
					type: "reset_stream",
					id: streamId,
					code: VarInt.from(0),
				});

				session.#sendStreams.delete(streamIdVal);
				session.#maybeDeleteStreamFlow(streamIdVal);
			},
			async close() {
				await Promise.race([
					session.#sendFrame({
						type: "stream",
						id: streamId,
						data: new Uint8Array(),
						fin: true,
					}),
					session.closed,
				]);

				session.#sendStreams.delete(streamIdVal);
				session.#maybeDeleteStreamFlow(streamIdVal);
			},
		});

		return writer;
	}

	#close(code: number, reason: string) {
		if (this.#idleTimer) {
			clearInterval(this.#idleTimer);
			this.#idleTimer = undefined;
		}
		this.#closedResolve({
			closeCode: code,
			reason,
		});

		// Fail active streams so consumers unblock
		try {
			this.#incomingBidirectionalStreams.close();
		} catch {}
		try {
			this.#incomingUnidirectionalStreams.close();
		} catch {}
		for (const c of this.#sendStreams.values()) {
			try {
				c.error(this.#closed);
			} catch {}
		}
		for (const c of this.#recvStreams.values()) {
			try {
				c.error(this.#closed);
			} catch {}
		}
		this.#sendStreams.clear();
		this.#recvStreams.clear();

		// Close per-stream credits before clearing the map
		for (const flow of this.#streamFlow.values()) {
			flow.sendCredit.close();
		}
		this.#streamFlow.clear();

		// Close global credits so blocked claim() calls reject
		this.#connCredit.close();
		this.#bidiStreamCredit.close();
		this.#uniStreamCredit.close();
		this.#recvBiCredit.close();
		this.#recvUniCredit.close();
	}

	close(info?: { closeCode?: number; reason?: string }) {
		if (this.#closed) return;

		const code = info?.closeCode ?? 0;
		const reason = info?.reason ?? "";

		this.#sendPriorityFrame({
			type: "connection_close",
			code: VarInt.from(code),
			reason,
		});

		setTimeout(() => {
			this.#ws.close();
		}, 100);

		this.#close(code, reason);
	}

	get congestionControl(): string {
		return "default";
	}
}
