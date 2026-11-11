import assert from "node:assert/strict";
import { setImmediate } from "node:timers/promises";
import Session from "../../src/session.ts";

const url = process.argv[2];
if (!url || !globalThis.gc) throw new Error("requires a peer URL and --expose-gc");
const session = new Session(url, { serverCertificateDisableVerify: true });
await session.ready;
const incoming = session.incomingUnidirectionalStreams.getReader();
const trigger = session.datagrams.writable.getWriter();

for (const pipe of [false, true]) {
	const started = Promise.withResolvers<void>();
	const result = (async () => {
		const { value: readable } = await incoming.read();
		if (!readable) throw new Error("no stream");
		if (pipe) {
			const observed = readable.pipeThrough(
				new TransformStream<Uint8Array, Uint8Array>({
					transform(chunk, controller) {
						started.resolve();
						controller.enqueue(chunk);
					},
				}),
			);
			return new Response(observed).arrayBuffer();
		}
		const reader = readable.getReader();
		await reader.read();
		started.resolve();
		return reader.read();
	})();
	// Wait for the first byte, then collect while only the consumer's result is retained.
	await started.promise;
	await setImmediate();
	globalThis.gc();
	await setImmediate();
	await trigger.write(new Uint8Array([1]));
	const value = await result;
	if (pipe) assert.deepEqual(new Uint8Array(value as ArrayBuffer), new Uint8Array([1, 2]));
	else assert.deepEqual((value as ReadableStreamReadResult<Uint8Array>).value, new Uint8Array([2]));
}

session.close();
await session.closed;
