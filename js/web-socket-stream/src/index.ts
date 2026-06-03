/** A ponyfill/polyfill for the WHATWG [`WebSocketStream`][spec] API.
 *
 * `WebSocketStream` exposes a WebSocket as a pair of streams —
 * `{ readable, writable }` — with backpressure on the writable. It currently
 * ships only in Chromium. This package wraps a plain `WebSocket` to present the
 * same surface (`opened`, `closed`, `close()`), applying write backpressure by
 * polling `bufferedAmount` against a configurable high-water mark.
 *
 * Note: a ponyfill can only *approximate* backpressure via `bufferedAmount`;
 * the native API observes the real send buffer. When the native API is present,
 * prefer it via {@link openWebSocketStream}.
 *
 * [spec]: https://github.com/ricea/websocketstream-explainer
 */

/** The data yielded by the readable / accepted by the writable. */
export type WebSocketStreamData = Uint8Array | string;

/** Resolved value of {@link WebSocketStreamLike.opened}. */
export interface WebSocketStreamOpenEvent {
	readable: ReadableStream<WebSocketStreamData>;
	writable: WritableStream<WebSocketStreamData>;
	extensions: string;
	protocol: string;
}

/** Resolved value of {@link WebSocketStreamLike.closed}. */
export interface WebSocketStreamCloseEvent {
	closeCode: number;
	reason: string;
}

/** Argument to {@link WebSocketStreamLike.close}. */
export interface WebSocketStreamCloseInfo {
	closeCode?: number;
	reason?: string;
}

/** The common shape implemented by both the native API and this ponyfill. */
export interface WebSocketStreamLike {
	readonly url: string;
	readonly opened: Promise<WebSocketStreamOpenEvent>;
	readonly closed: Promise<WebSocketStreamCloseEvent>;
	close(closeInfo?: WebSocketStreamCloseInfo): void;
	/** Ponyfill-only: resize the write-backpressure high-water mark at runtime
	 *  (see {@link WebSocketStream.setHighWaterMark}). Absent on the native API,
	 *  which sizes its own send buffer — callers should treat it as optional. */
	setHighWaterMark?(bytes: number): void;
}

/** The slice of the `WebSocket` API this ponyfill relies on. Both the browser
 *  `WebSocket` and Node's `ws` satisfy it. */
export interface WebSocketLike {
	binaryType: string;
	readonly bufferedAmount: number;
	readonly readyState: number;
	readonly protocol: string;
	readonly extensions: string;
	send(data: string | ArrayBufferLike | ArrayBufferView): void;
	close(code?: number, reason?: string): void;
	onopen: ((ev: unknown) => void) | null;
	onmessage: ((ev: { data: unknown }) => void) | null;
	onerror: ((ev: unknown) => void) | null;
	onclose: ((ev: { code?: number; reason?: string }) => void) | null;
}

/** Constructor for a {@link WebSocketLike} (e.g. the global `WebSocket` or `ws`). */
export type WebSocketConstructor = new (url: string, protocols?: string | string[]) => WebSocketLike;

export interface WebSocketStreamOptions {
	/** Subprotocols to advertise via `Sec-WebSocket-Protocol`. */
	protocols?: string[];
	/** Abort the connection. */
	signal?: AbortSignal;
	/** Ponyfill-only: the `WebSocket` implementation to use. Defaults to
	 *  `globalThis.WebSocket`. Pass Node's `ws` when there is no global. */
	webSocket?: WebSocketConstructor;
	/** Ponyfill-only: write backpressure kicks in once `bufferedAmount` exceeds
	 *  this many bytes. Defaults to 64 KiB. */
	highWaterMark?: number;
}

const OPEN = 1;
const DEFAULT_HIGH_WATER_MARK = 64 * 1024;

/** WebSocket close codes outside 1000 / 3000–4999 are rejected by `close()`. */
function validCloseCode(code: number | undefined): code is number {
	return code !== undefined && (code === 1000 || (code >= 3000 && code <= 4999));
}

/** Resolve once `bufferedAmount` drains below the mark (write backpressure).
 *  The mark is read live via `highWaterMark()` on every check, so a runtime
 *  resize (see {@link WebSocketStream.setHighWaterMark}) takes effect mid-drain.
 *  Returns `undefined` synchronously when there's already room — no microtask
 *  on the hot path, so `writer.ready` stays resolved when the socket is keeping up. */
function drain(ws: WebSocketLike, highWaterMark: () => number): Promise<void> | undefined {
	if (ws.bufferedAmount <= highWaterMark()) return undefined;
	return (async () => {
		while (ws.bufferedAmount > highWaterMark()) {
			if (ws.readyState > OPEN) return; // CLOSING/CLOSED — stop blocking
			await new Promise((resolve) => setTimeout(resolve, 10));
		}
	})();
}

/** A `WebSocketStream` implemented over a plain `WebSocket`. */
export class WebSocketStream implements WebSocketStreamLike {
	readonly url: string;
	readonly opened: Promise<WebSocketStreamOpenEvent>;
	readonly closed: Promise<WebSocketStreamCloseEvent>;
	#ws: WebSocketLike;
	#highWaterMark: number;

	constructor(url: string, options: WebSocketStreamOptions = {}) {
		this.url = url;

		const Ctor = options.webSocket ?? (globalThis as { WebSocket?: WebSocketConstructor }).WebSocket;
		if (!Ctor) {
			throw new Error("No WebSocket implementation found; pass options.webSocket");
		}
		const ws = new Ctor(url, options.protocols);
		ws.binaryType = "arraybuffer";
		this.#ws = ws;
		this.#highWaterMark = Math.max(1, Math.floor(options.highWaterMark ?? DEFAULT_HIGH_WATER_MARK));

		const opened = Promise.withResolvers<WebSocketStreamOpenEvent>();
		const closed = Promise.withResolvers<WebSocketStreamCloseEvent>();
		this.opened = opened.promise;
		this.closed = closed.promise;
		// Avoid unhandled-rejection noise if a caller only awaits `closed`.
		this.opened.catch(() => {});

		let controller: ReadableStreamDefaultController<WebSocketStreamData> | undefined;
		const readable = new ReadableStream<WebSocketStreamData>({
			start: (c) => {
				controller = c;
			},
			cancel: () => ws.close(),
		});

		const writable = new WritableStream<WebSocketStreamData>({
			write: (chunk) => {
				ws.send(chunk);
				return drain(ws, () => this.#highWaterMark);
			},
			// A WebSocket has no half-close, so closing/aborting the writable closes
			// the whole socket. Without this, `writer.close()` would leave it open.
			close: () => ws.close(),
			abort: () => ws.close(),
		});

		ws.onopen = () => {
			opened.resolve({ readable, writable, extensions: ws.extensions, protocol: ws.protocol });
		};
		ws.onmessage = (event) => {
			const data = event.data;
			if (typeof data === "string") {
				controller?.enqueue(data);
			} else if (data instanceof ArrayBuffer) {
				controller?.enqueue(new Uint8Array(data));
			} else if (ArrayBuffer.isView(data)) {
				const view = data as ArrayBufferView;
				controller?.enqueue(new Uint8Array(view.buffer, view.byteOffset, view.byteLength));
			}
		};
		ws.onerror = () => {
			const err = new Error("WebSocket connection error");
			opened.reject(err);
			try {
				controller?.error(err);
			} catch {}
		};
		ws.onclose = (event) => {
			opened.reject(new Error("WebSocket closed before opening"));
			try {
				controller?.close();
			} catch {}
			closed.resolve({ closeCode: event.code ?? 1006, reason: event.reason ?? "" });
		};

		const { signal } = options;
		if (signal) {
			if (signal.aborted) ws.close();
			else signal.addEventListener("abort", () => ws.close(), { once: true });
		}
	}

	close(closeInfo: WebSocketStreamCloseInfo = {}): void {
		if (validCloseCode(closeInfo.closeCode)) {
			this.#ws.close(closeInfo.closeCode, closeInfo.reason);
		} else {
			// A reason can't be sent without a valid code, so drop both.
			this.#ws.close();
		}
	}

	/** The current write-backpressure high-water mark, in bytes. */
	get highWaterMark(): number {
		return this.#highWaterMark;
	}

	/** Resize the write-backpressure high-water mark at runtime (bytes).
	 *
	 * Set this to roughly the bandwidth-delay product (RTT × estimated
	 * throughput): large enough to keep the socket busy, small enough that
	 * queued bytes can still be reprioritized rather than committed to the OS
	 * send buffer. Takes effect immediately, including for an in-progress drain.
	 * Clamped to a minimum of 1 byte. */
	setHighWaterMark(bytes: number): void {
		this.#highWaterMark = Math.max(1, Math.floor(bytes));
	}
}

/** Open a `WebSocketStream`, preferring the native API when available and
 *  falling back to the {@link WebSocketStream} ponyfill otherwise. Passing
 *  `options.webSocket` forces the ponyfill (the native API can't use an injected
 *  socket). */
export function openWebSocketStream(url: string | URL, options: WebSocketStreamOptions = {}): WebSocketStreamLike {
	const href = typeof url === "string" ? url : url.toString();
	const Native = (globalThis as { WebSocketStream?: typeof WebSocketStream }).WebSocketStream;
	// Only delegate to a *genuinely* native global — if `install()` put this very
	// ponyfill on the global, fall through so ponyfill-only options (e.g.
	// highWaterMark) aren't dropped on the floor.
	if (Native && Native !== WebSocketStream && !options.webSocket) {
		return new Native(href, { protocols: options.protocols, signal: options.signal });
	}
	return new WebSocketStream(href, options);
}

/** Install {@link WebSocketStream} as the global `WebSocketStream` if the
 *  platform doesn't ship one. Returns `true` if installed, `false` if a native
 *  (or previously installed) implementation already existed. */
export function install(): boolean {
	if ("WebSocketStream" in globalThis) return false;
	(globalThis as { WebSocketStream?: typeof WebSocketStream }).WebSocketStream = WebSocketStream;
	return true;
}

export default WebSocketStream;
