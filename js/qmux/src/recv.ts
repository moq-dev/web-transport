/** The receive half of a multiplexed stream.
 *
 * Buffers incoming STREAM data and hands it to the application's
 * `ReadableStream` *on demand* (pull-driven). `onConsume` fires only when bytes
 * are actually delivered to the reader — never on mere receipt — so flow-control
 * credit (MAX_STREAM_DATA) tracks the application's read rate. Combined with the
 * sender claiming credit before sending, this bounds how far the peer can run
 * ahead of a slow reader: it can buffer at most one receive window of undelivered
 * data before its credit is exhausted.
 *
 * The default queuing strategy (high-water mark 1) means at most one delivered
 * chunk sits in the `ReadableStream` itself; everything else waits in our buffer
 * until pulled.
 */
export class RecvStream {
	#queue: Uint8Array[] = [];
	#fin = false;
	#error?: Error;
	#wake?: () => void;

	/** The application-facing readable. */
	readonly readable: ReadableStream<Uint8Array>;

	/**
	 * @param onConsume Invoked with each chunk's byte length as it is delivered
	 *   to the reader. Drives MAX_STREAM_DATA.
	 * @param onCancel Invoked when the application cancels the readable (→ STOP_SENDING).
	 */
	constructor(onConsume: (bytes: number) => void, onCancel: () => void) {
		this.readable = new ReadableStream<Uint8Array>({
			pull: async (controller) => {
				while (this.#queue.length === 0) {
					if (this.#error) {
						controller.error(this.#error);
						return;
					}
					if (this.#fin) {
						controller.close();
						return;
					}
					await new Promise<void>((resolve) => {
						this.#wake = resolve;
					});
				}
				const chunk = this.#queue.shift() as Uint8Array;
				controller.enqueue(chunk);
				// Credit must track *delivery*, not receipt — do NOT move onConsume
				// into push(). This is the line that bounds the peer to one receive
				// window ahead of a slow reader; crediting on receipt would let the
				// buffer grow without bound.
				onConsume(chunk.byteLength);
			},
			cancel: () => onCancel(),
		});
	}

	/** Buffer a received chunk for delivery. Ignored after FIN/error. */
	push(chunk: Uint8Array): void {
		if (this.#fin || this.#error) return;
		this.#queue.push(chunk);
		this.#signal();
	}

	/** Mark end-of-stream; the readable closes once buffered data drains. */
	finish(): void {
		this.#fin = true;
		this.#signal();
	}

	/** Abort the readable, discarding undelivered buffered data. */
	error(err: Error): void {
		if (this.#error) return;
		this.#error = err;
		this.#queue = [];
		this.#signal();
	}

	#signal(): void {
		const wake = this.#wake;
		if (wake) {
			this.#wake = undefined;
			wake();
		}
	}
}
