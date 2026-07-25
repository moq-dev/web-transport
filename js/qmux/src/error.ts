/** A session-level failure, shaped like the `WebTransportError` that the
 *  WebTransport spec requires `WebTransport.closed` to reject with.
 *
 *  The native `WebTransportError` constructor only exists where the browser
 *  already implements WebTransport — which is precisely where this polyfill is
 *  not used — so mint our own rather than feature-detect a global that will not
 *  be there. `name`, `source`, and `streamErrorCode` match the interface, so a
 *  consumer branching on `err.source === "session"` works against the native API
 *  and this polyfill alike; `cause` carries the underlying failure (idle timeout,
 *  socket error, decode failure, ...).
 */
export class SessionError extends Error implements Pick<WebTransportError, "source" | "streamErrorCode"> {
	readonly source = "session" as const;
	readonly streamErrorCode = null;

	constructor(message: string, options?: { cause?: unknown }) {
		super(message, options);
		this.name = "WebTransportError";
	}
}

/** A stream reset by the peer, shaped like the `WebTransportError` the WebTransport spec
 *  requires a reset stream to error with.
 *
 *  Same reasoning as {@link SessionError}: mint our own rather than reach for a global the
 *  runnning environment does not have. The code the peer sent is the whole point of this
 *  error, so it goes in `streamErrorCode` where a consumer already looks for it, rather
 *  than being formatted into the message and lost.
 */
export class StreamError extends Error implements Pick<WebTransportError, "source" | "streamErrorCode"> {
	readonly source = "stream" as const;
	readonly streamErrorCode: number;

	constructor(message: string, code: number) {
		super(`${message}: ${code}`);
		this.name = "WebTransportError";
		this.streamErrorCode = code;
	}
}

/** The application error code a reset `reason` carries onto the wire.
 *
 *  The WebTransport spec derives the RESET_STREAM / STOP_SENDING code from the reason passed
 *  to `abort()` / `cancel()`: a `WebTransportError`'s `streamErrorCode`, or 0 for anything
 *  else, including no reason at all. Matched by field rather than by class so both a native
 *  `WebTransportError` and one of ours work.
 */
export function resetCode(reason: unknown): number {
	if (typeof reason !== "object" || reason === null) return 0;

	const { streamErrorCode } = reason as { streamErrorCode?: unknown };
	if (typeof streamErrorCode !== "number" || !Number.isFinite(streamErrorCode)) return 0;

	// The IDL type is `unsigned long`, so wrap like the platform would.
	return streamErrorCode >>> 0;
}
