import type { Version } from "./session.ts";
import Session from "./session.ts";

export type { Config, SessionOptions, Version } from "./session.ts";

/** Options forwarded to every `Session` constructed by [[install]]. */
export interface InstallOptions {
	/** ALPN -> wire-format version(s). See [[SessionOptions.versions]]. */
	versions?: Record<string, Version | Version[] | null>;
	/** Also offer the bare version ALPNs. See [[SessionOptions.withoutProtocol]]. */
	withoutProtocol?: boolean;
}

/** Install `Session` as the global `WebTransport` if the platform doesn't ship one.
 *
 * The `versions` map and `withoutProtocol` flag are forwarded to every
 * `Session` constructed via the polyfill. Callers that pass only explicit
 * `{qmux-VV}.{alpn}` pairs in their `protocols` list can omit the map.
 *
 * Returns `true` if the polyfill was installed, `false` if `globalThis.WebTransport`
 * already existed.
 */
export function install(options?: InstallOptions): boolean {
	if ("WebTransport" in globalThis) return false;
	const versions = options?.versions;
	const withoutProtocol = options?.withoutProtocol;
	// biome-ignore lint/suspicious/noExplicitAny: polyfill — extending Session to match the WebTransport constructor signature
	(globalThis as any).WebTransport = class extends Session {
		constructor(url: string | URL, options?: WebTransportOptions) {
			super(url, { ...options, versions, withoutProtocol });
		}
	};
	return true;
}

export default Session;
