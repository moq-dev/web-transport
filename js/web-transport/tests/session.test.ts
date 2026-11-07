import { beforeAll, expect, test } from "bun:test";
import { createSocket } from "node:dgram";
import { join } from "node:path";
import napi from "../napi.cjs";
import Session, { install, Server, WebTransportError } from "../src/index.ts";

let url: string;
let pem: Buffer;
let peerExecutable: string;

// The peer of each client session, in connect order.
const accepted: Session[] = [];
const waiters: ((session: Session) => void)[] = [];

beforeAll(async () => {
	// One PEM holding both the certificate and its key; the server reads each block out of it.
	const certificate = Bun.spawnSync(
		[
			"openssl",
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
		],
		{ stderr: "pipe" },
	);
	if (!certificate.success) throw new Error(`openssl failed: ${certificate.stderr}`);
	pem = certificate.stdout;
	const metadata = Bun.spawnSync(["cargo", "metadata", "--no-deps", "--format-version", "1"]);
	if (!metadata.success) throw new Error(`cargo metadata failed: ${metadata.stderr}`);
	peerExecutable = join(
		JSON.parse(metadata.stdout.toString()).target_directory,
		"debug",
		"examples",
		`session-peer${process.platform === "win32" ? ".exe" : ""}`,
	);

	const port = await freePort();
	const server = Server.bind(`127.0.0.1:${port}`, pem, pem);
	url = `https://127.0.0.1:${port}`;

	void (async () => {
		for (;;) {
			const request = await server.accept();
			if (!request) return;
			const session = await request.ok();
			const waiter = waiters.shift();
			if (waiter) waiter(session);
			else accepted.push(session);
		}
	})();
});

async function freePort(): Promise<number> {
	const socket = createSocket("udp4");
	await new Promise<void>((resolve) => socket.bind(0, "127.0.0.1", resolve));
	const { port } = socket.address();
	await new Promise<void>((resolve) => socket.close(resolve));
	return port;
}

async function connect(): Promise<{ client: Session; peer: Session }> {
	const client = new Session(url, { serverCertificateDisableVerify: true });
	await client.ready;
	const peer = accepted.shift() ?? (await new Promise<Session>((resolve) => waiters.push(resolve)));
	return { client, peer };
}

async function rejection(promise: Promise<unknown>): Promise<WebTransportError> {
	const err = await promise.then(
		() => {
			throw new Error("expected a rejection");
		},
		(err: unknown) => err,
	);
	expect(err).toBeInstanceOf(WebTransportError);
	expect(err).toBeInstanceOf(DOMException);
	expect((err as WebTransportError).name).toBe("WebTransportError");
	return err as WebTransportError;
}

test("install() defines WebTransportError alongside WebTransport", () => {
	const global = globalThis as { WebTransport?: unknown; WebTransportError?: unknown };
	try {
		expect(install()).toBe(true);
		expect(global.WebTransport).toBe(Session);
		expect(global.WebTransportError).toBe(WebTransportError);
	} finally {
		delete global.WebTransport;
		delete global.WebTransportError;
	}
});

test("a failed TLS connection rejects ready and closed with a session error", async () => {
	// The loopback server has a self-signed certificate, which system roots reject.
	const client = new Session(url);
	const closed = rejection(client.closed);
	const ready = await rejection(client.ready);
	expect(ready.source).toBe("session");
	expect(ready.streamErrorCode).toBeNull();
	expect(await closed).toBe(ready);
});

test("local and peer closes preserve nonzero application codes and reasons", async () => {
	const { client, peer } = await connect();
	const info = { closeCode: 42, reason: "done" };
	client.close(info);
	client.close({ closeCode: 43, reason: "ignored" });
	expect(await client.closed).toEqual(info);
	expect(await peer.closed).toEqual(info);
});

for (const side of ["client", "server"] as const) {
	for (const orderly of [false, true]) {
		test(`${orderly ? "an orderly HTTP/3 shutdown resolves" : "an underlying connection failure rejects"} the ${side} Session.closed`, async () => {
			const child = Bun.spawn([peerExecutable, ...(side === "server" ? [url] : [])], {
				env: {
					...process.env,
					WEB_TRANSPORT_TEST_PEM: pem.toString(),
					WEB_TRANSPORT_TEST_ORDERLY_CLOSE: orderly ? "1" : undefined,
				},
				stdin: "pipe",
				stdout: "pipe",
				stderr: "pipe",
			});
			let session: Session | undefined;
			try {
				if (side === "client") {
					const { value } = await child.stdout.getReader().read();
					if (!value) throw new Error("peer did not print its address");
					session = new Session(new TextDecoder().decode(value).trim(), {
						serverCertificateDisableVerify: true,
					});
					await session.ready;
				} else {
					session = accepted.shift() ?? (await new Promise<Session>((resolve) => waiters.push(resolve)));
				}
				const closed = orderly ? session.closed : rejection(session.closed);
				await session.datagrams.writable.getWriter().write(new Uint8Array([1]));
				if (orderly) expect(await closed).toEqual({ closeCode: 0, reason: "" });
				else {
					const err = (await closed) as WebTransportError;
					expect(err.source).toBe("session");
					expect(err.streamErrorCode).toBeNull();
					// A late local close must not turn an already observed failure into success.
					session.close();
					expect(await rejection(session.closed)).toBe(err);
				}
				child.stdin.end();
				expect(await child.exited).toBe(0);
				expect(await new Response(child.stderr).text()).toBe("");
			} finally {
				session?.close();
				child.kill();
				await child.exited;
			}
		});
	}
}

test("a peer's RESET_STREAM errors the reader with its code", async () => {
	const { client, peer } = await connect();

	const writer = (await peer.createUnidirectionalStream()).getWriter();
	await writer.write(new Uint8Array([1]));

	const { value: readable } = await client.incomingUnidirectionalStreams.getReader().read();
	if (!readable) throw new Error("no stream");
	const reader = readable.getReader();
	expect((await reader.read()).value).toEqual(new Uint8Array([1]));

	await writer.abort(new WebTransportError("cancel", { streamErrorCode: 7 }));

	const err = await rejection(reader.read());
	expect(err.source).toBe("stream");
	expect(err.streamErrorCode).toBe(7);
	expect((await rejection(reader.closed)).streamErrorCode).toBe(7);

	client.close();
});

test("a peer's STOP_SENDING errors the idle writer with its code", async () => {
	const { client, peer } = await connect();

	const stream = await client.createBidirectionalStream();
	const writer = stream.writable.getWriter();
	await writer.write(new Uint8Array([1]));

	const { value: bi } = await peer.incomingBidirectionalStreams.getReader().read();
	if (!bi) throw new Error("no stream");
	const reader = bi.readable.getReader();
	expect((await reader.read()).value).toEqual(new Uint8Array([1]));

	// A field-shaped reason carries its code too, as a library without this class would pass.
	await reader.cancel(Object.assign(new Error("stop"), { streamErrorCode: 9 }));

	// Nothing is written after the stop, so only the watcher can surface it.
	const err = await rejection(writer.closed);
	expect(err.source).toBe("stream");
	expect(err.streamErrorCode).toBe(9);
	expect((await rejection(writer.write(new Uint8Array([2])))).streamErrorCode).toBe(9);

	client.close();
});

test("a session close fails its streams and new streams with a session error", async () => {
	const { client, peer } = await connect();

	const stream = await client.createBidirectionalStream();
	const reader = stream.readable.getReader();
	const writer = stream.writable.getWriter();
	await writer.write(new Uint8Array([1]));
	await peer.incomingBidirectionalStreams.getReader().read();

	peer.close({ closeCode: 1, reason: "bye" });
	await client.closed;

	for (const err of [
		await rejection(reader.read()),
		await rejection(writer.closed),
		await rejection(client.createUnidirectionalStream()),
	]) {
		expect(err.source).toBe("session");
		expect(err.streamErrorCode).toBeNull();
	}
});

test("streamErrorCode is clamped like a WebIDL [Clamp] unsigned long", () => {
	const code = (streamErrorCode: number) => new WebTransportError("", { streamErrorCode }).streamErrorCode;
	expect(code(-1)).toBe(0);
	expect(code(2 ** 32)).toBe(0xffffffff);
	expect(code(2.5)).toBe(2);
	expect(code(3.5)).toBe(4);
	expect(code(Number.NaN)).toBe(0);
});

test("an abandoned writer still finishes its stream", async () => {
	const { client, peer } = await connect();

	// Leave nothing referencing the writable once this returns.
	await (async () => {
		const writer = (await client.createUnidirectionalStream()).getWriter();
		await writer.write(new Uint8Array([1]));
	})();

	const { value: readable } = await peer.incomingUnidirectionalStreams.getReader().read();
	if (!readable) throw new Error("no stream");
	const reader = readable.getReader();
	expect((await reader.read()).value).toEqual(new Uint8Array([1]));

	// Collecting the writable drops the native stream, which sends its implicit FIN. Finalizers
	// run between turns of the event loop, so collect until the FIN arrives.
	let done = false;
	const read = reader.read().then((result) => {
		done = result.done;
	});
	while (!done) {
		Bun.gc(true);
		await new Promise(setImmediate);
	}
	await read;

	client.close();
});

test("cancelling a pending read stops the peer with the requested code", async () => {
	const { client, peer } = await connect();
	try {
		const writer = (await peer.createUnidirectionalStream()).getWriter();
		await writer.write(new Uint8Array([1]));
		const { value: readable } = await client.incomingUnidirectionalStreams.getReader().read();
		if (!readable) throw new Error("no stream");
		const reader = readable.getReader();
		await reader.read();
		const read = reader.read();
		// Let the native call park before cancellation has to wake it.
		await new Promise(setImmediate);
		await reader.cancel(new WebTransportError("cancel", { streamErrorCode: 17 }));
		expect((await read).done).toBe(true);
		const err = await rejection(writer.closed);
		expect(err.source).toBe("stream");
		expect(err.streamErrorCode).toBe(17);
	} finally {
		client.close();
	}
});

test("concurrent read requests keep pulling through data and FIN", async () => {
	const { client, peer } = await connect();
	try {
		const writer = (await peer.createUnidirectionalStream()).getWriter();
		await writer.write(new Uint8Array([1]));
		const { value: readable } = await client.incomingUnidirectionalStreams.getReader().read();
		if (!readable) throw new Error("no stream");
		const reader = readable.getReader();
		expect((await reader.read()).value).toEqual(new Uint8Array([1]));
		const first = reader.read();
		const second = reader.read();
		await writer.write(new Uint8Array([2]));
		expect((await first).value).toEqual(new Uint8Array([2]));
		await writer.write(new Uint8Array([3]));
		expect((await second).value).toEqual(new Uint8Array([3]));
		await writer.close();
		expect((await reader.read()).done).toBe(true);
	} finally {
		client.close();
	}
});

test("native cancellation is idempotent after FIN and RESET_STREAM", async () => {
	for (const reset of [false, true]) {
		const client = await napi.NapiClient.disableVerify().connect(url);
		const peer = accepted.shift() ?? (await new Promise<Session>((resolve) => waiters.push(resolve)));
		try {
			const writer = (await peer.createUnidirectionalStream()).getWriter();
			await writer.write(new Uint8Array([1]));
			const recv = await client.acceptUni();
			await recv.read(65536);
			if (reset) await writer.abort(new WebTransportError("reset", { streamErrorCode: 7 }));
			else await writer.close();
			await recv.read(65536);
			await recv.stop(17);
			await recv.stop(17);
		} finally {
			client.close(0, "done");
			await client.closed();
		}
	}
});

test("an awaited read and pipe retain an active receiver through collection", async () => {
	for (const pipe of [false, true]) {
		const { client, peer } = await connect();
		try {
			const writer = (await peer.createUnidirectionalStream()).getWriter();
			await writer.write(new Uint8Array([1]));
			const started = Promise.withResolvers<void>();
			const result = (async () => {
				const { value: readable } = await client.incomingUnidirectionalStreams.getReader().read();
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
			// Force collection while only the consumer's pending result is retained.
			await started.promise;
			await new Promise(setImmediate);
			Bun.gc(true);
			await new Promise(setImmediate);
			await writer.write(new Uint8Array([2]));
			await writer.close();
			const value = await result;
			if (pipe) expect(new Uint8Array(value as ArrayBuffer)).toEqual(new Uint8Array([1, 2]));
			else expect((value as ReadableStreamReadResult<Uint8Array>).value).toEqual(new Uint8Array([2]));
		} finally {
			client.close();
		}
	}
});

test("Node retains an awaited read and pipe through collection", async () => {
	const child = Bun.spawn(["node", "--expose-gc", join(import.meta.dir, "fixtures/active-reader.ts"), url], {
		stdout: "pipe",
		stderr: "pipe",
	});
	let peer: Session | undefined;
	try {
		peer = accepted.shift() ?? (await new Promise<Session>((resolve) => waiters.push(resolve)));
		const datagrams = peer.datagrams.readable.getReader();
		for (let i = 0; i < 2; i++) {
			const writer = (await peer.createUnidirectionalStream()).getWriter();
			await writer.write(new Uint8Array([1]));
			await datagrams.read();
			await writer.write(new Uint8Array([2]));
			await writer.close();
		}
		expect(await child.exited).toBe(0);
		expect(await new Response(child.stderr).text()).toBe("");
	} finally {
		peer?.close();
		child.kill();
		await child.exited;
	}
});
