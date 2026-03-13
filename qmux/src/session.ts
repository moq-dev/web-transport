import type { Version, TransportParams } from "./frame.ts";
import * as Frame from "./frame.ts";
import { MAX_FRAME_PAYLOAD, RECOMMENDED_TRANSPORT_PARAMS, DEFAULT_TRANSPORT_PARAMS } from "./frame.ts";
import * as Stream from "./stream.ts";
import { VarInt } from "./varint.ts";
import { Credit } from "./credit.ts";

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
	/** Application-level subprotocols to request during the WebSocket handshake.
	 *
	 * Each protocol is prefixed with `webtransport.` and `qmux-00.` on the wire.
	 */
	protocols?: string[];
}

const PREFIX_WEBTRANSPORT = "webtransport.";
const PREFIX_QMUX = "qmux-00.";

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

	#version: Version = "webtransport";

	/** The negotiated application-level subprotocol, or empty string if none.
	 *
	 * The prefix is stripped; this returns only the application protocol name.
	 */
	#protocol = "";
	get protocol(): string {
		return this.#protocol;
	}

	readonly ready: Promise<void>;
	#readyResolve!: () => void;
	readonly closed: Promise<WebTransportCloseInfo>;
	#closedResolve!: (info: WebTransportCloseInfo) => void;

	readonly incomingBidirectionalStreams: ReadableStream<WebTransportBidirectionalStream>;
	#incomingBidirectionalStreams!: ReadableStreamDefaultController<WebTransportBidirectionalStream>;
	readonly incomingUnidirectionalStreams: ReadableStream<ReadableStream<Uint8Array>>;
	#incomingUnidirectionalStreams!: ReadableStreamDefaultController<ReadableStream<Uint8Array>>;

	// TODO: Implement datagrams
	readonly datagrams = new Datagrams();

	// Flow control state
	#ourParams: TransportParams = { ...RECOMMENDED_TRANSPORT_PARAMS };
	#peerParams: TransportParams = { ...DEFAULT_TRANSPORT_PARAMS };
	#paramsReceived = false;

	// Connection-level send credit
	#connCredit: Credit;

	// Connection-level recv flow control
	#recvDataOffset = 0n;
	#recvDataMax = 0n;

	// Per-stream flow control
	#streamFlow = new Map<bigint, StreamFlowState>();

	// Stream count tracking via Credit
	#bidiStreamCredit: Credit;
	#uniStreamCredit: Credit;
	#recvBidiOpened = 0n;
	#recvUniOpened = 0n;

	constructor(url: string | URL, options?: SessionOptions) {
		if (options?.requireUnreliable) {
			throw new Error("not allowed to use WebSocket; requireUnreliable is true");
		}

		if (options?.serverCertificateHashes) {
			console.warn("serverCertificateHashes is not supported; trying anyway");
		}

		url = Session.#convertToWebSocketUrl(url);

		// Offer both qmux-00 and webtransport prefixed protocols, preferring qmux-00
		const appProtocols = options?.protocols ?? [];
		const prefixed = new Set<string>(["qmux-00", "webtransport"]);
		for (const p of appProtocols) {
			const stripped = p.startsWith(PREFIX_WEBTRANSPORT)
				? p.slice(PREFIX_WEBTRANSPORT.length)
				: p.startsWith(PREFIX_QMUX)
					? p.slice(PREFIX_QMUX.length)
					: p;
			prefixed.add(`${PREFIX_QMUX}${stripped}`);
			prefixed.add(`${PREFIX_WEBTRANSPORT}${stripped}`);
		}
		this.#ws = new WebSocket(url, [...prefixed]);

		// Initialize credits — will be adjusted when version is detected
		this.#connCredit = new Credit(0n);
		this.#bidiStreamCredit = new Credit(0n);
		this.#uniStreamCredit = new Credit(0n);

		this.ready = new Promise((resolve) => {
			this.#readyResolve = resolve;
		});

		this.closed = new Promise((resolve) => {
			this.#closedResolve = resolve;
		});

		this.#ws.binaryType = "arraybuffer";
		this.#ws.onopen = () => {
			// Detect version from the negotiated subprotocol
			const raw = this.#ws.protocol;
			if (raw.startsWith(PREFIX_QMUX)) {
				this.#version = "qmux-00";
				this.#protocol = raw.slice(PREFIX_QMUX.length);
			} else if (raw.startsWith(PREFIX_WEBTRANSPORT)) {
				this.#version = "webtransport";
				this.#protocol = raw.slice(PREFIX_WEBTRANSPORT.length);
			} else if (raw === "qmux-00") {
				this.#version = "qmux-00";
				this.#protocol = "";
			} else {
				this.#version = "webtransport";
				this.#protocol = "";
			}

			if (this.#version === "qmux-00") {
				this.#recvDataMax = this.#ourParams.initialMaxData;
				this.#sendTransportParameters();
			} else {
				// No flow control for WebTransport — set unlimited
				this.#connCredit = new Credit(BigInt(Number.MAX_SAFE_INTEGER));
				this.#bidiStreamCredit = new Credit(BigInt(Number.MAX_SAFE_INTEGER));
				this.#uniStreamCredit = new Credit(BigInt(Number.MAX_SAFE_INTEGER));
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
		try {
			const frame = Frame.decode(data, this.#version);
			if (frame !== null) {
				this.#recvFrame(frame);
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
	}

	async #claimSendCredit(streamId: bigint, desired: bigint): Promise<bigint> {
		const flow = this.#streamFlow.get(streamId);
		if (!flow) return desired;

		while (true) {
			// 1. Try stream credit
			const streamClaimed = flow.sendCredit.tryClaim(desired);
			if (streamClaimed === 0n) {
				if (this.#closed) throw this.#closeReason || new Error("Connection closed");
				await flow.sendCredit.claim(desired);
				flow.sendCredit.release(desired > flow.sendCredit.available + desired ? desired : desired);
				// Release what claim() took and retry the full loop
				// to coordinate with connection credit
				const c = flow.sendCredit.tryClaim(desired);
				if (c > 0n) flow.sendCredit.release(c);
				continue;
			}

			// 2. Try connection credit
			const connClaimed = this.#connCredit.tryClaim(streamClaimed);
			if (connClaimed === 0n) {
				flow.sendCredit.release(streamClaimed);
				if (this.#closed) throw this.#closeReason || new Error("Connection closed");
				const c = await this.#connCredit.claim(1n);
				this.#connCredit.release(c);
				continue;
			}

			// Return excess stream credit if connection had less
			if (connClaimed < streamClaimed) {
				flow.sendCredit.release(streamClaimed - connClaimed);
			}

			return connClaimed;
		}
	}

	#accountRecv(streamId: bigint, bytes: number) {
		if (this.#version !== "qmux-00" || bytes === 0) return;

		this.#recvDataOffset += BigInt(bytes);

		const flow = this.#streamFlow.get(streamId);
		if (flow) {
			flow.recvOffset += BigInt(bytes);
		}
	}

	#accountConsumed(streamId: bigint, bytes: number) {
		if (this.#version !== "qmux-00" || bytes === 0) return;

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
		let totalConsumed = 0n;
		for (const flow of this.#streamFlow.values()) {
			totalConsumed += flow.recvConsumed;
		}

		if (totalConsumed >= threshold) {
			const newMax = this.#recvDataOffset + window;
			if (newMax > this.#recvDataMax) {
				this.#recvDataMax = newMax;
				this.#sendPriorityFrame({ type: "max_data", max: newMax });
			}
		}
	}

	#maybeSendMaxStreamData(streamId: bigint, flow: StreamFlowState) {
		const id = new Stream.Id(VarInt.from(streamId));

		const initialWindow = id.dir === Stream.Dir.Bi
			? this.#ourParams.initialMaxStreamDataBidiRemote
			: this.#ourParams.initialMaxStreamDataUni;

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

	async #handleStreamFrame(frame: Frame.Data) {
		if (frame.data.byteLength > MAX_FRAME_PAYLOAD) {
			this.close({ closeCode: 1002, reason: "frame too large" });
			return;
		}

		const streamId = frame.id.value.value;

		if (!frame.id.canRecv(this.#isServer)) {
			throw new Error("Invalid stream ID direction");
		}

		// Account for received bytes
		this.#accountRecv(streamId, frame.data.byteLength);

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
			if (this.#version === "qmux-00") {
				if (frame.id.dir === Stream.Dir.Bi) {
					this.#recvBidiOpened += 1n;
				} else {
					this.#recvUniOpened += 1n;
				}
			}

			// Initialize flow control state for new stream
			if (this.#version === "qmux-00") {
				const recvMax = frame.id.dir === Stream.Dir.Bi
					? this.#ourParams.initialMaxStreamDataBidiRemote
					: this.#ourParams.initialMaxStreamDataUni;

				// For send side on bidi: peer's bidi_local is our send limit
				const sendMax = frame.id.dir === Stream.Dir.Bi
					? this.#peerParams.initialMaxStreamDataBidiLocal
					: 0n;

				this.#streamFlow.set(streamId, {
					sendCredit: new Credit(sendMax),
					recvMax,
					recvOffset: BigInt(frame.data.byteLength),
					recvConsumed: 0n,
				});
			}

			const session = this;

			const reader = new ReadableStream<Uint8Array>({
				start: (controller) => {
					stream = controller;
					session.#recvStreams.set(streamId, controller);
				},
				cancel: () => {
					session.#sendPriorityFrame({
						type: "stop_sending",
						id: frame.id,
						code: VarInt.from(0),
					});

					session.#recvStreams.delete(streamId);
				},
			});

			if (!stream) {
				throw new Error("ReadableStream didn't call start");
			}

			if (frame.id.dir === Stream.Dir.Bi) {
				// Incoming bidirectional stream
				const writer = new WritableStream<Uint8Array>({
					start: (controller) => {
						session.#sendStreams.set(streamId, controller);
					},
					write: async (chunk) => {
						await Promise.race([
							session.#sendStreamDataWithFlowControl(frame.id, streamId, chunk),
							session.closed,
						]);
					},
					abort: (e) => {
						console.warn("abort", e);
						session.#sendPriorityFrame({
							type: "reset_stream",
							id: frame.id,
							code: VarInt.from(0),
						});

						session.#sendStreams.delete(streamId);
						session.#streamFlow.delete(streamId);
					},
					close: async () => {
						await Promise.race([
							session.#sendFrame({
								type: "stream",
								id: frame.id,
								data: new Uint8Array(),
								fin: true,
							}),
							session.closed,
						]);

						session.#sendStreams.delete(streamId);
					},
				});

				this.#incomingBidirectionalStreams.enqueue({ readable: reader, writable: writer });
			} else {
				this.#incomingUnidirectionalStreams.enqueue(reader);
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
		}
	}

	#handleResetStream(frame: Frame.ResetStream) {
		const streamId = frame.id.value.value;
		const stream = this.#recvStreams.get(streamId);
		if (!stream) return;

		stream.error(new Error(`RESET_STREAM: ${frame.code.value}`));
		this.#recvStreams.delete(streamId);
		this.#streamFlow.delete(streamId);
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
	}

	#sendTransportParameters() {
		const frame: Frame.TransportParameters = {
			type: "transport_parameters",
			params: this.#ourParams,
		};
		const encoded = Frame.encode(frame, this.#version);
		this.#ws.send(encoded);
	}

	async #sendStreamDataWithFlowControl(id: Stream.Id, streamId: bigint, data: Uint8Array) {
		for (let offset = 0; offset < data.byteLength; ) {
			const remaining = data.byteLength - offset;
			const chunkMax = Math.min(remaining, MAX_FRAME_PAYLOAD);

			// Claim flow control credit (stream + connection)
			const allowed = await this.#claimSendCredit(streamId, BigInt(chunkMax));
			const sendable = Number(allowed);

			const chunk = data.subarray(offset, offset + sendable);

			await this.#sendFrame({
				type: "stream",
				id,
				data: chunk,
				fin: false,
			});

			offset += sendable;
		}
	}

	async #sendStreamData(id: Stream.Id, data: Uint8Array) {
		const streamId = id.value.value;
		if (this.#version === "qmux-00") {
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

		const chunk = Frame.encode(frame, this.#version);
		this.#ws.send(chunk);
	}

	#sendPriorityFrame(frame: Frame.Any) {
		const chunk = Frame.encode(frame, this.#version);
		this.#ws.send(chunk);
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
		if (this.#version === "qmux-00") {
			this.#streamFlow.set(streamIdVal, {
				sendCredit: new Credit(this.#peerParams.initialMaxStreamDataBidiRemote),
				recvMax: this.#ourParams.initialMaxStreamDataBidiLocal,
				recvOffset: 0n,
				recvConsumed: 0n,
			});
		}

		const session = this;

		const writer = new WritableStream<Uint8Array>({
			start: (controller) => {
				session.#sendStreams.set(streamIdVal, controller);
			},
			write: async (chunk) => {
				await Promise.race([
					session.#sendStreamData(streamId, chunk),
					session.closed,
				]);
			},
			abort: (e) => {
				console.warn("abort", e);
				session.#sendPriorityFrame({
					type: "reset_stream",
					id: streamId,
					code: VarInt.from(0),
				});

				session.#sendStreams.delete(streamIdVal);
				session.#streamFlow.delete(streamIdVal);
			},
			close: async () => {
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
			},
		});

		const reader = new ReadableStream<Uint8Array>({
			start: (controller) => {
				session.#recvStreams.set(streamIdVal, controller);
			},
			cancel: async () => {
				session.#sendPriorityFrame({
					type: "stop_sending",
					id: streamId,
					code: VarInt.from(0),
				});

				session.#recvStreams.delete(streamIdVal);
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
		if (this.#version === "qmux-00") {
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
				await Promise.race([
					session.#sendStreamData(streamId, chunk),
					session.closed,
				]);
			},
			abort(e) {
				console.warn("abort", e);
				session.#sendPriorityFrame({
					type: "reset_stream",
					id: streamId,
					code: VarInt.from(0),
				});

				session.#sendStreams.delete(streamIdVal);
				session.#streamFlow.delete(streamIdVal);
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
			},
		});

		return writer;
	}

	#close(code: number, reason: string) {
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
		this.#streamFlow.clear();

		// Close all credits so blocked claim() calls reject
		this.#connCredit.close();
		this.#bidiStreamCredit.close();
		this.#uniStreamCredit.close();
		for (const flow of this.#streamFlow.values()) {
			flow.sendCredit.close();
		}
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
