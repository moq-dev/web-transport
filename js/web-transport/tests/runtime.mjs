import assert from "node:assert/strict";
import { createSocket } from "node:dgram";
import { readFile } from "node:fs/promises";
import { setImmediate } from "node:timers/promises";
import Session, { Server } from "@moq/web-transport";

const [mode, certPath, keyPath] = process.argv.slice(2);
assert.ok(mode === "natural" || mode === "forced");
assert.ok(!process.versions.bun, "the package must run under Node");

// Reserve an OS-selected UDP port, then hand it to the public server API, which
// does not expose the address selected by bind(...:0).
const socket = createSocket("udp4");
const port = await new Promise((resolve, reject) => {
	socket.once("error", reject);
	socket.bind(0, "127.0.0.1", () => resolve(socket.address().port));
});
await new Promise((resolve) => socket.close(resolve));

const server = Server.bind(`127.0.0.1:${port}`, await readFile(certPath), await readFile(keyPath));
let client;
let peer;
try {
	console.log(`${mode}: connecting through the installed package`);
	const accepting = server.accept();
	client = new Session(`https://127.0.0.1:${port}/runtime`, { serverCertificateDisableVerify: true });
	const request = await accepting;
	assert.ok(request, "the server must receive the connection");
	assert.equal(new URL(await request.url).pathname, "/runtime");
	peer = await request.ok();
	await Promise.all([client.ready, peer.ready]);

	const incomingClient = client.incomingUnidirectionalStreams.getReader();
	const incomingPeer = peer.incomingUnidirectionalStreams.getReader();
	async function sendUni(from, incoming, text) {
		const writer = (await from.createUnidirectionalStream()).getWriter();
		try {
			await writer.write(new TextEncoder().encode(text));
			await writer.close();
		} finally {
			writer.releaseLock();
		}
		const { value, done } = await incoming.read();
		assert.equal(done, false);
		assert.equal(await new Response(value).text(), text);
	}
	await sendUni(client, incomingPeer, "client unidirectional payload");
	await sendUni(peer, incomingClient, "server unidirectional payload");
	console.log(`${mode}: unidirectional streams passed`);

	const incomingBi = peer.incomingBidirectionalStreams.getReader();
	const bi = await client.createBidirectionalStream();
	const writer = bi.writable.getWriter();
	await writer.write(new TextEncoder().encode("request"));
	await writer.close();
	writer.releaseLock();
	const accepted = await incomingBi.read();
	assert.equal(accepted.done, false);
	assert.equal(await new Response(accepted.value.readable).text(), "request");
	const reply = accepted.value.writable.getWriter();
	await reply.write(new TextEncoder().encode("response"));
	await reply.close();
	reply.releaseLock();
	assert.equal(await new Response(bi.readable).text(), "response");
	console.log(`${mode}: bidirectional streams passed`);

	const clientDatagrams = client.datagrams.readable.getReader();
	const peerDatagrams = peer.datagrams.readable.getReader();
	for (const [from, incoming, payload] of [
		[client, peerDatagrams, [1, 2, 3]],
		[peer, clientDatagrams, [4, 5, 6]],
	]) {
		assert.ok(from.datagrams.maxDatagramSize >= payload.length);
		const datagrams = from.datagrams.writable.getWriter();
		try {
			await datagrams.write(Uint8Array.from(payload));
		} finally {
			datagrams.releaseLock();
		}
		const received = await incoming.read();
		assert.equal(received.done, false);
		assert.deepEqual([...received.value], payload);
	}
	console.log(`${mode}: datagrams passed`);

	if (mode === "forced") {
		// Exercise native runtime shutdown with network operations still parked.
		// A use-after-free or other native crash must reach the supervising process.
		void incomingPeer.read();
		void peerDatagrams.read();
		await setImmediate();
		console.log("forced: exiting with pending native operations");
		process.exit(0);
	}

	client.close({ closeCode: 37, reason: "runtime test complete" });
	const [local, remote] = await Promise.all([client.closed, peer.closed]);
	assert.equal(typeof local.closeCode, "number");
	assert.equal(typeof local.reason, "string");
	assert.deepEqual(remote, { closeCode: 37, reason: "runtime test complete" });
	for (const reader of [incomingClient, incomingPeer, incomingBi, clientDatagrams, peerDatagrams]) {
		assert.equal((await reader.read()).done, true, "closure must wake blocked readers");
		reader.releaseLock();
	}
	console.log("natural: session closure passed; waiting for natural process exit");
} finally {
	client?.close();
	peer?.close();
	server.close();
}
// Deliberately do not call process.exit(): lingering native handles must fail the
// supervising timeout even after every protocol assertion succeeds.
