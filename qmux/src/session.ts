import type { Version } from "./frame.ts";
import * as Frame from "./frame.ts";
import { MAX_FRAME_PAYLOAD } from "./frame.ts";
import * as Stream from "./stream.ts";
import { VarInt } from "./varint.ts";

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
	#readyResolve: () => void;
	readonly closed: Promise<WebTransportCloseInfo>;
	#closedResolve: (info: WebTransportCloseInfo) => void;

	readonly incomingBidirectionalStreams: ReadableStream<WebTransportBidirectionalStream>;
	#incomingBidirectionalStreams!: ReadableStreamDefaultController<WebTransportBidirectionalStream>;
	readonly incomingUnidirectionalStreams: ReadableStream<ReadableStream<Uint8Array>>;
	#incomingUnidirectionalStreams!: ReadableStreamDefaultController<ReadableStream<Uint8Array>>;

	// TODO: Implement datagrams
	readonly datagrams = new Datagrams();

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

		const ready = Promise.withResolvers<void>();
		this.ready = ready.promise;
		this.#readyResolve = ready.resolve;

		const closed = Promise.withResolvers<WebTransportCloseInfo>();
		this.closed = closed.promise;
		this.#closedResolve = closed.resolve;

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

			// QMux requires TRANSPORT_PARAMETERS as the first frame.
			if (this.#version === "qmux-00") {
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
		} else {
			const exhaustive: never = frame;
			throw new Error(`Unknown frame type: ${exhaustive}`);
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
					},
				});

				this.#incomingBidirectionalStreams.enqueue({ readable: reader, writable: writer });
			} else {
				this.#incomingUnidirectionalStreams.enqueue(reader);
			}
		}

		if (frame.data.byteLength > 0) {
			stream.enqueue(frame.data);
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
		// QX_TRANSPORT_PARAMETERS frame: type (0x3f5153300d0a0d0a) + length (0)
		const frameType = VarInt.from(0x3f5153300d0a0d0an);
		const length = VarInt.from(0);

		let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
		buffer = frameType.encode(buffer);
		buffer = length.encode(buffer);

		this.#ws.send(buffer);
	}

	async #sendStreamData(id: Stream.Id, data: Uint8Array) {
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

		const streamId = Stream.Id.create(this.#nextBiStreamId++, Stream.Dir.Bi, this.#isServer);

		const writer = new WritableStream<Uint8Array>({
			start: (controller) => {
				this.#sendStreams.set(streamId.value.value, controller);
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

				this.#sendStreams.delete(streamId.value.value);
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

				this.#sendStreams.delete(streamId.value.value);
			},
		});

		const reader = new ReadableStream<Uint8Array>({
			start: (controller) => {
				this.#recvStreams.set(streamId.value.value, controller);
			},
			cancel: async () => {
				this.#sendPriorityFrame({
					type: "stop_sending",
					id: streamId,
					code: VarInt.from(0),
				});

				this.#recvStreams.delete(streamId.value.value);
			},
		});

		return { readable: reader, writable: writer };
	}

	async createUnidirectionalStream(): Promise<WritableStream<Uint8Array>> {
		await this.ready;

		if (this.#closed) {
			throw this.#closed;
		}

		const streamId = Stream.Id.create(this.#nextUniStreamId++, Stream.Dir.Uni, this.#isServer);

		const session = this;

		const writer = new WritableStream<Uint8Array>({
			start: (controller) => {
				session.#sendStreams.set(streamId.value.value, controller);
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

				session.#sendStreams.delete(streamId.value.value);
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

				session.#sendStreams.delete(streamId.value.value);
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
