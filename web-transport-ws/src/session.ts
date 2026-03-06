import * as Frame from "./frame.ts";
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
export interface WebTransportWsOptions extends WebTransportOptions {
	/** Application-level subprotocols to request during the WebSocket handshake.
	 *
	 * Each protocol is prefixed with `webtransport.` on the wire.
	 */
	protocols?: string[];
}

const PREFIX = "webtransport.";

export default class WebTransportWs implements WebTransport {
	#ws: WebSocket;
	#isServer = false;
	#closed?: Error;
	#closeReason?: Error;

	#sendStreams = new Map<bigint, WritableStreamDefaultController>();
	#recvStreams = new Map<bigint, ReadableStreamDefaultController<Uint8Array>>();

	#nextUniStreamId = 0n;
	#nextBiStreamId = 0n;

	/** The negotiated application-level subprotocol, or empty string if none.
	 *
	 * The `webtransport.` prefix is stripped; this returns only the application protocol name.
	 */
	readonly protocol: string;

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

	constructor(url: string | URL, options?: WebTransportWsOptions) {
		if (options?.requireUnreliable) {
			throw new Error("not allowed to use WebSocket; requireUnreliable is true");
		}

		if (options?.serverCertificateHashes) {
			console.warn("serverCertificateHashes is not supported; trying anyway");
		}

		url = WebTransportWs.#convertToWebSocketUrl(url);

		// Prefix each application protocol with `webtransport.` on the wire.
		const prefixed = (options?.protocols ?? []).map((p) => `${PREFIX}${p}`);
		const wsProtocols = [...new Set(["webtransport", ...prefixed])];
		this.#ws = new WebSocket(url, wsProtocols);

		this.ready = new Promise((resolve) => {
			this.#readyResolve = resolve;
		});

		this.closed = new Promise((resolve) => {
			this.#closedResolve = resolve;
		});

		this.protocol = "";

		this.#ws.binaryType = "arraybuffer";
		this.#ws.onopen = () => {
			// The browser returns the single selected subprotocol (no commas).
			// Strip the `webtransport:` prefix to get the application protocol.
			const raw = this.#ws.protocol;
			const selected = raw.startsWith(PREFIX) ? raw.slice(PREFIX.length) : "";
			(this as { protocol: string }).protocol = selected;
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
			const frame = Frame.decode(data);
			this.#recvFrame(frame);
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
						await Promise.race([
							this.#sendFrame({
								type: "stream",
								id: frame.id,
								data: chunk,
								fin: false,
							}),
							this.closed,
						]);
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

	async #sendFrame(frame: Frame.Any) {
		// Add some backpressure so we don't saturate the connection
		while (this.#ws.bufferedAmount > 64 * 1024) {
			await new Promise((resolve) => setTimeout(resolve, 10));
		}

		const chunk = Frame.encode(frame);
		this.#ws.send(chunk);
	}

	#sendPriorityFrame(frame: Frame.Any) {
		const chunk = Frame.encode(frame);
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
				await Promise.race([
					this.#sendFrame({
						type: "stream",
						id: streamId,
						data: chunk,
						fin: false,
					}),
					this.closed,
				]);
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
				await Promise.race([
					session.#sendFrame({
						type: "stream",
						id: streamId,
						data: chunk,
						fin: false,
					}),
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
