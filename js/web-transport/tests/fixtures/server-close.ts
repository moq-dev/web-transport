import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createSocket, type RemoteInfo } from "node:dgram";
import Session, { type Request, Server } from "../../src/index.ts";

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
const socket = createSocket("udp4");
await new Promise<void>((resolve) => socket.bind(0, "127.0.0.1", resolve));
const { port } = socket.address();
await new Promise<void>((resolve) => socket.close(resolve));
const server = Server.bind("127.0.0.1:" + port, pem, pem);
const peerReady = Promise.withResolvers<Session>();
let result: Request | null | undefined;
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

// Hold a second client's first server flight. Quinn only sends it after native accept has
// consumed the incoming connection, and no WebTransport request can finish without it.
const proxy = createSocket("udp4");
await new Promise<void>((resolve) => proxy.bind(0, "127.0.0.1", resolve));
const pending = Promise.withResolvers<void>();
const flights: Buffer[] = [];
let remoteClient: RemoteInfo | undefined;
let holding = true;
proxy.on("message", (data, remote) => {
	if (remote.port === port) {
		assert(remoteClient);
		if (holding) {
			if (flights.length === 0) {
				assert.equal(data.readUInt32BE(1), 1); // QUIC v1, not version negotiation.
				assert.equal(data[0] & 0xb0, 0x80); // Long-header Initial, not Retry (fixed bit may be greased).
			}
			flights.push(data);
			pending.resolve();
		} else {
			proxy.send(data, remoteClient.port, remoteClient.address);
		}
	} else {
		remoteClient = remote;
		proxy.send(data, port, "127.0.0.1");
	}
});
const stalled = new Session("https://127.0.0.1:" + proxy.address().port, {
	serverCertificateDisableVerify: true,
});
const rejected = assert.rejects(stalled.ready);
await pending.promise;
server.close();
server.close();
await loop;
assert.equal(result, null);
assert.equal(await server.accept(), null);

// Deliver the held flight so the client's handshake can observe the server's close.
holding = false;
assert(remoteClient);
for (const flight of flights) proxy.send(flight, remoteClient.port, remoteClient.address);
await rejected;
await assert.rejects(stalled.closed);
await new Promise<void>((resolve) => proxy.close(resolve));

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
