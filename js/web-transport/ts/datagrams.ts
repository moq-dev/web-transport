import type { NapiSession } from "../index.js";

export class Datagrams implements WebTransportDatagramDuplexStream {
	readonly readable: ReadableStream<Uint8Array>;
	readonly writable: WritableStream<Uint8Array>;

	// Required by the interface but not meaningfully configurable here.
	incomingHighWaterMark = 1;
	incomingMaxAge: number | null = null;
	outgoingHighWaterMark = 1;
	outgoingMaxAge: number | null = null;

	#session: NapiSession;

	constructor(session: NapiSession) {
		this.#session = session;

		this.readable = new ReadableStream({
			async pull(controller) {
				try {
					const data = await session.recvDatagram();
					controller.enqueue(new Uint8Array(data));
				} catch {
					controller.close();
				}
			},
		});

		this.writable = new WritableStream({
			write(chunk) {
				session.sendDatagram(Buffer.from(chunk));
			},
		});
	}

	get maxDatagramSize(): number {
		return this.#session.maxDatagramSize();
	}
}
