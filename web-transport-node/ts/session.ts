import { NapiClient, type NapiRecvStream, type NapiSendStream, type NapiSession } from "../index.js";
import { Datagrams } from "./datagrams.ts";

function wrapRecvStream(recv: NapiRecvStream): ReadableStream<Uint8Array> {
	return new ReadableStream({
		async pull(controller) {
			const chunk = await recv.read(65536);
			if (chunk) {
				controller.enqueue(new Uint8Array(chunk));
			} else {
				controller.close();
			}
		},
		cancel() {
			recv.stop(0).catch(() => {});
		},
	});
}

function wrapSendStream(send: NapiSendStream): WritableStream<Uint8Array> {
	return new WritableStream({
		async write(chunk) {
			await send.write(Buffer.from(chunk));
		},
		async close() {
			await send.finish();
		},
		async abort() {
			await send.reset(0);
		},
	});
}

export default class Session implements WebTransport {
	readonly ready: Promise<void>;
	readonly closed: Promise<WebTransportCloseInfo>;
	readonly datagrams: WebTransportDatagramDuplexStream;

	#session: NapiSession | undefined;
	#pendingClose: { closeCode: number; reason: string } | undefined;
	#incomingBidirectionalStreams: ReadableStream<WebTransportBidirectionalStream> | undefined;
	#incomingUnidirectionalStreams: ReadableStream<ReadableStream<Uint8Array>> | undefined;

	// Construct from URL (client-side polyfill)
	constructor(url: string | URL, options?: WebTransportOptions);
	// Construct from existing NapiSession (server-side)
	constructor(session: NapiSession);
	constructor(urlOrSession: string | URL | NapiSession, options?: WebTransportOptions) {
		const ready = Promise.withResolvers<void>();
		const closed = Promise.withResolvers<WebTransportCloseInfo>();
		this.ready = ready.promise;
		this.closed = closed.promise;

		// Check if we got an existing NapiSession (server-side path)
		if (typeof urlOrSession === "object" && !(urlOrSession instanceof URL)) {
			this.#session = urlOrSession;
			this.datagrams = new Datagrams(urlOrSession);

			ready.resolve();

			urlOrSession.closed().then((info) => {
				closed.resolve({ closeCode: info.closeCode, reason: info.reason });
			});
		} else {
			// Client-side: create NapiClient and connect
			const url = typeof urlOrSession === "string" ? urlOrSession : urlOrSession.toString();

			// Provide a deferred Datagrams that works before connect resolves.
			// The real session will be bound once connect succeeds.
			this.datagrams = new DeferredDatagrams();

			const hashes = options?.serverCertificateHashes;
			let client: NapiClient;
			if (hashes && hashes.length > 0) {
				const buffers = hashes
					.filter((h): h is WebTransportHash & { value: BufferSource } => h.value != null)
					.map((h) => Buffer.from(h.value as ArrayBuffer));
				client = NapiClient.withCertificateHashes(buffers);
			} else {
				client = NapiClient.withSystemRoots();
			}

			client
				.connect(url)
				.then((session) => {
					// Check if close() was called before connect completed.
					if (this.#pendingClose) {
						session.close(this.#pendingClose.closeCode, this.#pendingClose.reason);
						(this.datagrams as DeferredDatagrams).fail(new Error("session closed before connect"));
						closed.resolve(this.#pendingClose);
						ready.reject(new Error("session closed before connect"));
						return;
					}

					this.#session = session;
					(this.datagrams as DeferredDatagrams).bind(session);
					ready.resolve();

					session.closed().then((info) => {
						closed.resolve({ closeCode: info.closeCode, reason: info.reason });
					});
				})
				.catch((err) => {
					(this.datagrams as DeferredDatagrams).fail(err instanceof Error ? err : new Error(String(err)));
					ready.reject(err instanceof Error ? err : new Error(String(err)));
					closed.resolve({ closeCode: 0, reason: String(err) });
				});
		}
	}

	get incomingBidirectionalStreams(): ReadableStream<WebTransportBidirectionalStream> {
		if (!this.#incomingBidirectionalStreams) {
			this.#incomingBidirectionalStreams = new ReadableStream({
				pull: async (controller) => {
					await this.ready;
					const session = this.#session;
					if (!session) {
						controller.close();
						return;
					}
					try {
						const bi = await session.acceptBi();
						const stream: WebTransportBidirectionalStream = {
							readable: wrapRecvStream(bi.takeRecv()),
							writable: wrapSendStream(bi.takeSend()),
						};
						controller.enqueue(stream);
					} catch {
						controller.close();
					}
				},
			});
		}
		return this.#incomingBidirectionalStreams;
	}

	get incomingUnidirectionalStreams(): ReadableStream<ReadableStream<Uint8Array>> {
		if (!this.#incomingUnidirectionalStreams) {
			this.#incomingUnidirectionalStreams = new ReadableStream({
				pull: async (controller) => {
					await this.ready;
					const session = this.#session;
					if (!session) {
						controller.close();
						return;
					}
					try {
						const recv = await session.acceptUni();
						controller.enqueue(wrapRecvStream(recv));
					} catch {
						controller.close();
					}
				},
			});
		}
		return this.#incomingUnidirectionalStreams;
	}

	async createBidirectionalStream(): Promise<WebTransportBidirectionalStream> {
		await this.ready;
		if (!this.#session) throw new Error("session not connected");
		const bi = await this.#session.openBi();
		return {
			readable: wrapRecvStream(bi.takeRecv()),
			writable: wrapSendStream(bi.takeSend()),
		};
	}

	async createUnidirectionalStream(): Promise<WritableStream<Uint8Array>> {
		await this.ready;
		if (!this.#session) throw new Error("session not connected");
		const send = await this.#session.openUni();
		return wrapSendStream(send);
	}

	close(info?: { closeCode?: number; reason?: string }): void {
		const closeCode = info?.closeCode ?? 0;
		const reason = info?.reason ?? "";
		if (this.#session) {
			this.#session.close(closeCode, reason);
		} else {
			// Connect hasn't completed yet — flag for when it does.
			this.#pendingClose = { closeCode, reason };
		}
	}

	get congestionControl(): WebTransportCongestionControl {
		return "default";
	}
}

/**
 * A WebTransportDatagramDuplexStream that works before the session is connected.
 * Readable/writable block until bind() is called with the real session.
 */
class DeferredDatagrams implements WebTransportDatagramDuplexStream {
	readonly readable: ReadableStream<Uint8Array>;
	readonly writable: WritableStream<Uint8Array>;

	incomingHighWaterMark = 1;
	incomingMaxAge: number | null = null;
	outgoingHighWaterMark = 1;
	outgoingMaxAge: number | null = null;

	#session: NapiSession | undefined;
	#bound = Promise.withResolvers<void>();

	constructor() {
		this.readable = new ReadableStream({
			pull: async (controller) => {
				// Wait for session to be bound
				if (!this.#session) {
					try {
						await this.#bound.promise;
					} catch {
						controller.close();
						return;
					}
				}
				if (!this.#session) {
					controller.close();
					return;
				}
				try {
					const data = await this.#session.recvDatagram();
					controller.enqueue(new Uint8Array(data));
				} catch {
					controller.close();
				}
			},
		});

		this.writable = new WritableStream({
			write: (chunk) => {
				if (!this.#session) throw new Error("session not connected");
				this.#session.sendDatagram(Buffer.from(chunk));
			},
		});
	}

	bind(session: NapiSession) {
		this.#session = session;
		this.#bound.resolve();
	}

	fail(_error: Error) {
		this.#bound.reject(_error);
	}

	get maxDatagramSize(): number {
		return this.#session?.maxDatagramSize() ?? 0;
	}
}
