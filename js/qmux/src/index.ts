import type { Version } from "./frame.ts";
import Session from "./session.ts";

export type { Version } from "./frame.ts";
export type { Config, SessionOptions } from "./session.ts";

/** Install Session as the global `WebTransport` if the platform doesn't ship one.
 *
 * The QMux version must be picked at install time — each Session is pinned to a
 * single version (mirrors the Rust API). Returns `true` if the polyfill was
 * installed, `false` if `globalThis.WebTransport` already existed.
 */
export function install(version: Version): boolean {
	if ("WebTransport" in globalThis) return false;
	// biome-ignore lint/suspicious/noExplicitAny: polyfill — extending Session to match the WebTransport constructor signature
	(globalThis as any).WebTransport = class extends Session {
		constructor(url: string | URL, options?: WebTransportOptions) {
			super(url, { ...options, version });
		}
	};
	return true;
}

export default Session;
