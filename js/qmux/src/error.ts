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
