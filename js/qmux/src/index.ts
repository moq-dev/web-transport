import type { SessionOptions } from "./session.ts";
import Session from "./session.ts";

export { SessionError, StreamError } from "./error.ts";
export type { AcceptOptions, Config, SendStreamOptions, SessionOptions, Version } from "./session.ts";
export { selectSubprotocol } from "./session.ts";

/** Options forwarded to every `Session` constructed by [[install]]. */
export interface InstallOptions extends Pick<SessionOptions, "versions" | "requireProtocol"> {}

/** Install `Session` as the global `WebTransport` if the platform doesn't ship one.
 *
 * The `versions` map and `requireProtocol` flag are forwarded to every
 * `Session` constructed via the polyfill. Callers that pass only explicit
 * `{qmux-VV}.{alpn}` pairs in their `protocols` list can omit the map.
 *
 * Returns `true` if the polyfill was installed, `false` if `globalThis.WebTransport`
 * already existed.
 */
export function install(options?: InstallOptions): boolean {
	if ("WebTransport" in globalThis) return false;
	const versions = options?.versions;
	const requireProtocol = options?.requireProtocol;
	globalThis.WebTransport = class extends Session {
		constructor(url: string | URL, options?: WebTransportOptions) {
			super(url, { ...options, versions, requireProtocol });
		}
	};
	return true;
}

export default Session;
