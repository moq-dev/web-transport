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

export default class Session {
	readonly ready: Promise<void>;
	readonly closed: Promise<WebTransportCloseInfo>;
	readonly datagrams: WebTransportDatagramDuplexStream;

	#session: NapiSession | undefined;
	#incomingBidirectionalStreams: ReadableStream<WebTransportBidirectionalStream> | undefined;
	#incomingUnidirectionalStreams: ReadableStream<ReadableStream<Uint8Array>> | undefined;

	// Construct from URL (client-side polyfill)
	constructor(url: string | URL, options?: WebTransportOptions);
	// Construct from existing NapiSession (server-side)
	constructor(session: NapiSession);
	constructor(urlOrSession: string | URL | NapiSession, options?: WebTransportOptions) {
		let readyResolve: () => void;
		let readyReject: (err: Error) => void;
		this.ready = new Promise<void>((resolve, reject) => {
			readyResolve = resolve;
			readyReject = reject;
		});

		let closedResolve: (info: WebTransportCloseInfo) => void;
		this.closed = new Promise<WebTransportCloseInfo>((resolve) => {
			closedResolve = resolve;
		});

		// Check if we got an existing NapiSession (server-side path)
		if (typeof urlOrSession === "object" && !(urlOrSession instanceof URL)) {
			this.#session = urlOrSession;
			this.datagrams = new Datagrams(urlOrSession);

			// biome-ignore lint/style/noNonNullAssertion: assigned synchronously in Promise constructor
			readyResolve!();

			urlOrSession.closed().then((reason) => {
				// biome-ignore lint/style/noNonNullAssertion: assigned synchronously in Promise constructor
				closedResolve!({ closeCode: 0, reason });
			});
		} else {
			// Client-side: create NapiClient and connect
			const url = typeof urlOrSession === "string" ? urlOrSession : urlOrSession.toString();

			this.datagrams = undefined as unknown as WebTransportDatagramDuplexStream;

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
					this.#session = session;
					(this as { datagrams: WebTransportDatagramDuplexStream }).datagrams = new Datagrams(session);
					// biome-ignore lint/style/noNonNullAssertion: assigned synchronously in Promise constructor
					readyResolve!();

					session.closed().then((reason) => {
						// biome-ignore lint/style/noNonNullAssertion: assigned synchronously in Promise constructor
						closedResolve!({ closeCode: 0, reason });
					});
				})
				.catch((err) => {
					// biome-ignore lint/style/noNonNullAssertion: assigned synchronously in Promise constructor
					readyReject!(err instanceof Error ? err : new Error(String(err)));
					// biome-ignore lint/style/noNonNullAssertion: assigned synchronously in Promise constructor
					closedResolve!({ closeCode: 0, reason: String(err) });
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
		this.#session?.close(info?.closeCode ?? 0, info?.reason ?? "");
	}

	get congestionControl(): string {
		return "default";
	}
}
