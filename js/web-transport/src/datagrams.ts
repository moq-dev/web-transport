import type { NapiSession } from "../napi.cjs";

/**
 * Datagrams over a session, which may still be connecting.
 * Reads wait for the session and end if it fails; writes before it connects throw.
 */
export class Datagrams implements WebTransportDatagramDuplexStream {
	readonly readable: ReadableStream<Uint8Array>;
	readonly writable: WritableStream<Uint8Array>;

	// Required by the interface but not meaningfully configurable here.
	incomingHighWaterMark = 1;
	incomingMaxAge: number | null = null;
	outgoingHighWaterMark = 1;
	outgoingMaxAge: number | null = null;

	#session: NapiSession | undefined;

	constructor(session: NapiSession | Promise<NapiSession>) {
		if (session instanceof Promise) {
			// Registered first, so the session is set before anyone else observes it resolving.
			void session.then(
				(session) => {
					this.#session = session;
				},
				() => {},
			);
		} else {
			this.#session = session;
		}

		this.readable = new ReadableStream({
			async pull(controller) {
				try {
					const data = await (await session).recvDatagram();
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

	get maxDatagramSize(): number {
		return this.#session?.maxDatagramSize() ?? 0;
	}
}
