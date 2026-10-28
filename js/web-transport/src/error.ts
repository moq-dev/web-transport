import type { NapiStreamError } from "../napi.cjs";

/** Options for {@link WebTransportError}, matching W3C `WebTransportErrorOptions`. */
export interface WebTransportErrorOptions {
	source?: WebTransportErrorSource;
	streamErrorCode?: number | null;
}

/** The W3C `WebTransportError`, which Node and Bun do not define: a stream reset or stop, or a session failure. */
export class WebTransportError extends DOMException {
	readonly source: WebTransportErrorSource;
	readonly streamErrorCode: number | null;

	constructor(message = "", options: WebTransportErrorOptions = {}) {
		super(message, "WebTransportError");
		this.source = options.source ?? "stream";
		this.streamErrorCode = options.streamErrorCode ?? null;
	}
}

/** Convert a failure reported by the native binding. */
export function fromNapi(err: NapiStreamError): WebTransportError {
	return new WebTransportError(err.message, { source: err.source, streamErrorCode: err.streamErrorCode ?? null });
}

/** A session failure, as a closed session fails new streams. */
export function sessionError(cause: unknown): WebTransportError {
	return new WebTransportError(cause instanceof Error ? cause.message : String(cause), { source: "session" });
}

/**
 * The RESET_STREAM or STOP_SENDING code for an `abort()` / `cancel()` reason.
 *
 * The spec takes a `WebTransportError`'s `streamErrorCode` and sends 0 for anything else. The
 * field is read rather than the class, so an error shaped like one from another polyfill or a
 * library that cannot see this class still carries its code.
 */
export function resetCode(reason: unknown): number {
	if (typeof reason !== "object" || reason === null) return 0;
	const { streamErrorCode } = reason as { streamErrorCode?: unknown };
	if (typeof streamErrorCode !== "number") return 0;
	// `unsigned long`, wrapped as the platform would.
	return streamErrorCode >>> 0;
}
