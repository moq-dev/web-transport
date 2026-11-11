import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createSocket } from "node:dgram";
import { setTimeout as sleep } from "node:timers/promises";
import Session, { Server, type ServerOptions, type SessionOptions } from "../../src/index.ts";

const pem = execFileSync("openssl", [
	"req",
	"-x509",
	"-newkey",
	"ec",
	"-pkeyopt",
	"ec_paramgen_curve:prime256v1",
	"-nodes",
	"-days",
	"1",
	"-subj",
	"/CN=localhost",
	"-keyout",
	"-",
]);

async function freePort(): Promise<number> {
	const socket = createSocket("udp4");
	await new Promise<void>((resolve) => socket.bind(0, "127.0.0.1", resolve));
	const { port } = socket.address();
	await new Promise<void>((resolve) => socket.close(resolve));
	return port;
}

async function connect(serverOptions: ServerOptions | undefined, clientOptions: SessionOptions) {
	const port = await freePort();
	const server = Server.bind(`127.0.0.1:${port}`, pem, pem, serverOptions);
	const accepting = server.accept();
	const client = new Session(`https://127.0.0.1:${port}`, { serverCertificateDisableVerify: true, ...clientOptions });
	const request = await accepting;
	assert(request);
	const peer = await request.ok();
	await client.ready;
	server.close();
	return { client, peer };
}

// Default timeouts are 30 seconds, so expiring within this bound proves a configured value applied.
const EXPIRY_BOUND_MS = 10_000;

async function expectExpiry(serverOptions: ServerOptions | undefined, clientOptions: SessionOptions) {
	const { client, peer } = await connect(serverOptions, clientOptions);
	const start = performance.now();
	const [info] = await Promise.all([client.closed, peer.closed]);
	assert.match(info.reason ?? "", /timed out/);
	assert(performance.now() - start < EXPIRY_BOUND_MS);
}

// Wait several idle timeouts, then prove the session still carries data both ways.
async function expectAlive(serverOptions: ServerOptions | undefined, clientOptions: SessionOptions) {
	const { client, peer } = await connect(serverOptions, clientOptions);
	let closed = false;
	void client.closed.then(() => {
		closed = true;
	});
	await sleep(1_500);
	assert.equal(closed, false);

	const reader = peer.datagrams.readable.getReader();
	const writer = client.datagrams.writable.getWriter();
	await writer.write(new Uint8Array([1]));
	assert.deepEqual((await reader.read()).value, new Uint8Array([1]));
	reader.releaseLock();
	writer.releaseLock();
	client.close();
	await Promise.all([client.closed, peer.closed]);
}

await Promise.all([
	// A configured timeout on either endpoint applies to the connection.
	expectExpiry({ idleTimeout: 300 }, {}),
	expectExpiry(undefined, { idleTimeout: 300 }),
	// Disabling one endpoint's timeout leaves the peer's in effect.
	expectExpiry({ idleTimeout: 300 }, { idleTimeout: 0 }),
	// Keep-alive holds open a connection that would otherwise expire.
	expectAlive(undefined, { idleTimeout: 300, keepAliveInterval: 100 }),
	expectAlive({ idleTimeout: 300, keepAliveInterval: 100 }, {}),
	// A timeout disabled on both endpoints is not treated as an immediate expiry.
	expectAlive({ idleTimeout: 0 }, { idleTimeout: 0 }),
]);

for (const value of [-1, 1.5, Number.NaN, Number.POSITIVE_INFINITY, Number.MAX_SAFE_INTEGER + 1]) {
	const url = "https://127.0.0.1:1";
	assert.throws(() => new Session(url, { idleTimeout: value }), /idleTimeout must be/);
	assert.throws(() => new Session(url, { keepAliveInterval: value }), /keepAliveInterval must be/);
	assert.throws(() => Server.bind("127.0.0.1:0", pem, pem, { idleTimeout: value }), /idleTimeout must be/);
	assert.throws(
		() => Server.bind("127.0.0.1:0", pem, pem, { keepAliveInterval: value }),
		/keepAliveInterval must be/,
	);
}

console.log("idle");
