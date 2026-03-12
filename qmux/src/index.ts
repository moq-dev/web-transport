import WebTransportWs from "./session.ts";

// Install polyfill if WebTransport is not available, returning true if installed
export function install(): boolean {
	if ("WebTransport" in globalThis) return false;
	// biome-ignore lint/suspicious/noExplicitAny: polyfill
	(globalThis as any).WebTransport = WebTransportWs;
	return true;
}

export default WebTransportWs;
