import { setImmediate } from "node:timers/promises";
import Session from "../../src/session.ts";

const url = process.argv[2];
if (!url || !globalThis.gc) throw new Error("requires a peer URL and --expose-gc");
const session = new Session(url, { serverCertificateDisableVerify: true });
await session.ready;

await (async () => {
	const { value: readable } = await session.incomingUnidirectionalStreams.getReader().read();
	if (!readable) throw new Error("no stream");
	const reader = readable.getReader();
	await reader.read();
	void reader.read();
})();

// The peer closes the Session after observing STOP_SENDING from reader collection.
let closed = false;
const closure = session.closed.then(() => {
	closed = true;
});
while (!closed) {
	globalThis.gc();
	await setImmediate();
}
await closure;
