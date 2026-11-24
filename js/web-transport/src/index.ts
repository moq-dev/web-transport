import { WebTransportError } from "./error.ts";
import Session from "./session.ts";

// Install polyfill if WebTransport is not available, returning true if installed
export function install(): boolean {
	if ("WebTransport" in globalThis) return false;
	globalThis.WebTransport = Session;
	globalThis.WebTransportError ??= WebTransportError;
	return true;
}

export default Session;
export { WebTransportError, type WebTransportErrorOptions } from "./error.ts";
export { Request } from "./request.ts";
export { Server, type ServerOptions } from "./server.ts";
export type { SessionOptions, TransportOptions } from "./session.ts";
