import { Credit } from "./credit.ts";
import type { TransportParams, WireFormat } from "./frame.ts";
import * as Frame from "./frame.ts";
import { DEFAULT_TRANSPORT_PARAMS, isQmux, MAX_FRAME_PAYLOAD } from "./frame.ts";
import * as Stream from "./stream.ts";
import { VarInt } from "./varint.ts";

/** The QMux wire-format versions a caller can advertise.
 *
 * The legacy `webtransport` wire format only appears bare on the wire (never
 * prefixed) and is appended automatically by the polyfill, so it isn't a
 * valid value here.
 */
export type Version = Exclude<WireFormat, "webtransport">;

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

/** Options for opening a QMux Session over WebSocket. */
export interface SessionOptions extends WebTransportOptions {
	/** Application-level subprotocols to advertise via `Sec-WebSocket-Protocol`.
	 *
	 * Each entry is either:
	 *  - A bare ALPN (e.g. `"moq-lite-04"`). The polyfill resolves it via
	 *    [[SessionOptions.versions]] and emits the prefixed wire form
	 *    `{version}.{alpn}` (e.g. `"qmux-01.moq-lite-04"`).
	 *  - An explicit pair already prefixed with a QMux version
	 *    (e.g. `"qmux-00.moq-transport-17"`). Advertised as-is.
	 *
	 * Bare entries that have no matching `versions` entry throw at
	 * construction time.
	 */
	protocols?: string[];

	/** Maps each bare ALPN to the QMux wire-format version(s) it can ride on.
	 *
	 * Required for every bare entry in `protocols`; entries already in
	 * `{qmux-VV}.{alpn}` pair form bypass this map. Per-entry semantics:
	 *
	 *  - `Version`: advertise exactly `{version}.{alpn}`.
	 *  - `Version[]`: advertise one `{v}.{alpn}` per array entry in order.
	 *    Lets the server pick across multiple QMux drafts.
	 *  - `null`: advertise the ALPN under every QMux version this polyfill
	 *    knows about (currently qmux-01, then qmux-00). Useful when the app
	 *    doesn't care which draft and wants forward compatibility.
	 *
	 * The legacy `webtransport.{alpn}` pair form was used briefly during the
	 * web-transport-ws -> qmux transition; no production client depended on
	 * it, so this polyfill never emits it.
	 */
	versions?: Record<string, Version | Version[] | null>;

	/** Also offer the bare version ALPNs `qmux-01`, `qmux-00`, and
	 * `webtransport` (no application protocol attached). Use this when the
	 * peer might only know a wire-format version and not the application
	 * protocols you've configured — e.g. when talking to a relay that only
	 * speaks the legacy `webtransport` wire format. Defaults to `false`,
	 * which advertises only the configured prefixed pairs.
	 */
	withoutProtocol?: boolean;

	/** QMux flow control configuration. Only used for the QMux wire formats. */
	config?: Config;
}

/** Get the subprotocol prefix for a QMux wire-format version. */
function versionPrefix(version: Version): string {
	switch (version) {
		case "qmux-01":
			return "qmux-01.";
		case "qmux-00":
			return "qmux-00.";
	}
}

/** QMux versions recognized as prefixed ALPNs, newest first. */
const QMUX_VERSIONS = ["qmux-01", "qmux-00"] as const satisfies readonly Version[];

/** Convert `http(s)://` to `ws(s)://`. Pass-through for `ws(s)://` URLs. */
function toWebSocketUrl(url: string | URL): string {
	const u = typeof url === "string" ? new URL(url) : url;
	let scheme: string;
	switch (u.protocol) {
		case "https:":
		case "wss:":
			scheme = "wss:";
			break;
		case "http:":
		case "ws:":
			scheme = "ws:";
			break;
		default:
			throw new Error(`Unsupported protocol: ${u.protocol}`);
	}
	return `${scheme}//${u.host}${u.pathname}${u.search}`;
}

/** Bare version ALPNs appended when `withoutProtocol` is set. Newest first. */
const BARE_ALPNS = ["qmux-01", "qmux-00", "webtransport"] as const;

/** Resolve a `protocols` + `versions` pair to the wire subprotocol list.
 *
 * Expansion rules per bare entry's `versions` value: single `Version`
 * emits one pair; an array emits one pair per element; `null` emits one
 * pair per [[QMUX_VERSIONS]] entry. Entries already in `{qmux-VV}.{alpn}`
 * pair form pass through. When `withoutProtocol` is true, the bare version
 * ALPNs (`qmux-01`, `qmux-00`, `webtransport`) are appended after the
 * prefixed pairs for peers that don't pin an app protocol.
 *
 * Throws if any bare entry has no matching `versions` mapping.
 */
function resolveSubprotocols(
	protocols: readonly string[],
	versions: Readonly<Record<string, Version | Version[] | null>>,
	withoutProtocol: boolean,
): string[] {
	const out: string[] = [];
	for (const entry of protocols) {
		const known = QMUX_VERSIONS.find((v) => entry.startsWith(versionPrefix(v)));
		if (known !== undefined) {
			out.push(entry);
			continue;
		}
		if (!(entry in versions)) {
			throw new Error(
				`Sec-WebSocket-Protocol entry ${JSON.stringify(entry)} has no qmux prefix and no versions mapping`,
			);
		}
		const value = versions[entry];
		const expanded: readonly Version[] = value === null ? QMUX_VERSIONS : Array.isArray(value) ? value : [value];
		for (const v of expanded) {
			out.push(`${versionPrefix(v)}${entry}`);
		}
	}
	if (withoutProtocol) {
		out.push(...BARE_ALPNS);
	}
	return out;
}

/** Pick the QMux wire-format version from a negotiated subprotocol. */
function detectVersion(negotiated: string): WireFormat {
	for (const v of QMUX_VERSIONS) {
		if (negotiated === v || negotiated.startsWith(versionPrefix(v))) {
			return v;
		}
	}
	// Empty or unrecognized: fall back to the pre-QMux wire format.
	return "webtransport";
}

/** Strip the version prefix from a negotiated `Sec-WebSocket-Protocol` value.
 *
 * Returns the application protocol name, or `""` if only the bare version
 * ALPN was negotiated (or the value was empty/unknown). `webtransport` is
 * always bare on the wire, so it never yields an application protocol.
 */
function parseProtocol(raw: string, version: WireFormat): string {
	if (raw === "" || version === "webtransport") return "";
	if (raw === version) return "";
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

	// Default to the legacy wire format until the WebSocket opens and the
	// negotiated subprotocol tells us otherwise. #handleOpen overrides this
	// with the actual version derived from `ws.protocol`.
	#version: WireFormat = "webtransport";

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

	// Send credits start at the legacy wire format's "unlimited" values to
	// match the default #version. #handleOpen replaces them with QMux-shaped
	// zero-credits (waiting for TRANSPORT_PARAMETERS) when the negotiated
	// version turns out to be a QMux draft.
	#connCredit = new Credit(BigInt(Number.MAX_SAFE_INTEGER));

	// Connection-level recv flow control
	#recvDataOffset = 0n;
	#recvDataMax = 0n;
	#recvDataConsumed = 0n;

	// Per-stream flow control
	#streamFlow = new Map<bigint, StreamFlowState>();

	// Stream count tracking via Credit (for sending — peer's limits).
	// Initialized to "unlimited" matching the default webtransport version;
	// #handleOpen replaces them when a QMux draft is negotiated.
	#bidiStreamCredit = new Credit(BigInt(Number.MAX_SAFE_INTEGER));
	#uniStreamCredit = new Credit(BigInt(Number.MAX_SAFE_INTEGER));

	// Stream count tracking via Credit (for receiving — our limits)
	#recvBiCredit: Credit;
	#recvUniCredit: Credit;

	// QMux01 idle-timeout tracking (engaged once we've received the peer's params).
	#lastRecvAt = Date.now();
	#lastSendAt = Date.now();
	#nextPingSeq = 0;
	#idleTimer?: ReturnType<typeof setInterval>;

	/** Open a QMux session over WebSocket against `url`.
	 *
	 * The polyfill constructs the underlying `WebSocket` itself. Pass the
	 * application-level ALPNs in `options.protocols` plus a `versions`
	 * map saying which QMux wire-format version each bare ALPN rides on. The
	 * wire form `{qmux-VV}.{alpn}` is built automatically; entries already in
	 * pair form (e.g. `"qmux-00.moq-transport-17"`) pass through unchanged.
	 *
	 * Once the handshake completes, the QMux wire-format version is derived
	 * from the negotiated `Sec-WebSocket-Protocol`. `.protocol` exposes the
	 * application protocol with the QMux prefix stripped.
	 */
	constructor(url: string | URL, options?: SessionOptions) {
		if (options?.requireUnreliable) {
			throw new Error("not allowed to use WebSocket; requireUnreliable is true");
		}
		if (options?.serverCertificateHashes) {
			console.warn("serverCertificateHashes is not supported; trying anyway");
		}

		const subprotocols = resolveSubprotocols(
			options?.protocols ?? [],
			options?.versions ?? {},
			options?.withoutProtocol ?? false,
		);
		this.#ws = new WebSocket(toWebSocketUrl(url), subprotocols);

		// Merge user config with defaults
		this.#config = { ...DEFAULT_CONFIG, ...options?.config };
		this.#ourParams = configToTransportParams(this.#config);

		// Recv stream count limits are version-independent.
		this.#recvBiCredit = new Credit(this.#config.maxStreamsBidi);
		this.#recvUniCredit = new Credit(this.#config.maxStreamsUni);

		const ready = Promise.withResolvers<void>();
		this.ready = ready.promise;
		this.#readyResolve = ready.resolve;

		const closed = Promise.withResolvers<WebTransportCloseInfo>();
		this.closed = closed.promise;
		this.#closedResolve = closed.resolve;

		this.#ws.binaryType = "arraybuffer";
		this.#ws.onopen = () => this.#handleOpen();
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

	#handleOpen() {
		const version = detectVersion(this.#ws.protocol);
		this.#version = version;
		this.#protocol = parseProtocol(this.#ws.protocol, version);

		// QMux drafts wait for the peer's TRANSPORT_PARAMETERS before sending,
		// so reset the unlimited (webtransport-shaped) defaults to zero credits.
		if (isQmux(version)) {
			this.#connCredit.close();
			this.#bidiStreamCredit.close();
			this.#uniStreamCredit.close();
			this.#connCredit = new Credit(0n);
			this.#bidiStreamCredit = new Credit(0n);
			this.#uniStreamCredit = new Credit(0n);
			this.#recvDataMax = this.#ourParams.initialMaxData;
			this.#sendTransportParameters();
		}

		this.#readyResolve();
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

		// Close global credits so blocked claim() calls reject.
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
