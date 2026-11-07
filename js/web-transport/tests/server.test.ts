import { expect, test } from "bun:test";
import { execFileSync } from "node:child_process";

test("close stops a pending accept loop without blocking the JavaScript thread", () => {
	const entry = new URL("../src/index.ts", import.meta.url).href;
	// The timeout lives in the parent process: a synchronous native deadlock also blocks JS timers.
	const output = execFileSync(
		"node",
		[
			"--input-type=module",
			"--eval",
			`
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createSocket } from "node:dgram";
import Session, { Server } from ${JSON.stringify(entry)};

const pem = execFileSync("openssl", [
	"req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1",
	"-nodes", "-days", "1", "-subj", "/CN=localhost", "-keyout", "-",
]);
const socket = createSocket("udp4");
await new Promise(resolve => socket.bind(0, "127.0.0.1", resolve));
const { port } = socket.address();
await new Promise(resolve => socket.close(resolve));
const server = Server.bind("127.0.0.1:" + port, pem, pem);
const peerReady = Promise.withResolvers();
let result;
const loop = (async () => {
	for (;;) {
		result = await server.accept();
		if (!result) return;
		peerReady.resolve(await result.ok());
	}
})();
const client = new Session("https://127.0.0.1:" + port, { serverCertificateDisableVerify: true });
await client.ready;
const peer = await peerReady.promise;
await new Promise(setImmediate);
server.close();
server.close();
await loop;
assert.equal(result, null);
assert.equal(await server.accept(), null);

// Stopping the listener must not close sessions it already accepted.
const readable = peer.datagrams.readable.getReader();
const writable = client.datagrams.writable.getWriter();
await writable.write(new Uint8Array([7]));
assert.deepEqual((await readable.read()).value, new Uint8Array([7]));
readable.releaseLock();
writable.releaseLock();
client.close();
await Promise.all([client.closed, peer.closed]);
console.log("closed");
`,
		],
		{ encoding: "utf8", timeout: 5_000, stdio: ["ignore", "pipe", "pipe"] },
	);
	expect(output.trim()).toBe("closed");
}, 10_000);
