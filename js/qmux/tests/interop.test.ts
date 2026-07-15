import { expect, test } from "bun:test";
import { resolve } from "node:path";
import Session, { type Version } from "../src/session.ts";

const ROOT = resolve(import.meta.dir, "../../..");
const PROTOCOL = "qmux-interop";
const PAYLOAD_LEN = 1_200_000;
const CLIENT_SEED = 17;
const SERVER_SEED = 29;
const OPERATION_TIMEOUT_MS = 15_000;

function payload(seed: number): Uint8Array {
	const bytes = new Uint8Array(PAYLOAD_LEN);
	for (let index = 0; index < bytes.byteLength; index++) {
		bytes[index] = (index * 31 + seed) % 251;
	}
	return bytes;
}

function expectPayload(actual: Uint8Array, seed: number): void {
	expect(actual.byteLength).toBe(PAYLOAD_LEN);
	for (let index = 0; index < actual.byteLength; index++) {
		if (actual[index] !== (index * 31 + seed) % 251) {
			throw new Error(`payload mismatch at byte ${index}`);
		}
	}
}

async function withTimeout<T>(label: string, promise: Promise<T>, timeoutMs = OPERATION_TIMEOUT_MS): Promise<T> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	try {
		return await Promise.race([
			promise,
			new Promise<never>((_, reject) => {
				timer = setTimeout(() => reject(new Error(`timed out: ${label}`)), timeoutMs);
			}),
		]);
	} finally {
		if (timer !== undefined) clearTimeout(timer);
	}
}

async function readLine(stream: ReadableStream<Uint8Array>): Promise<string> {
	const reader = stream.getReader();
	const decoder = new TextDecoder();
	let buffered = "";
	try {
		while (true) {
			const { value, done } = await reader.read();
			if (done) throw new Error("Rust interop server exited before reporting its URL");
			buffered += decoder.decode(value, { stream: true });
			const newline = buffered.indexOf("\n");
			if (newline !== -1) return buffered.slice(0, newline).trim();
		}
	} finally {
		reader.releaseLock();
	}
}

async function readAll(stream: ReadableStream<Uint8Array>): Promise<Uint8Array> {
	const reader = stream.getReader();
	const chunks: Uint8Array[] = [];
	let length = 0;
	try {
		while (true) {
			const { value, done } = await reader.read();
			if (done) break;
			chunks.push(value);
			length += value.byteLength;
		}
	} finally {
		reader.releaseLock();
	}

	const result = new Uint8Array(length);
	let offset = 0;
	for (const chunk of chunks) {
		result.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return result;
}

async function runVersion(version: Version): Promise<void> {
	const server = Bun.spawn(
		["cargo", "run", "--quiet", "-p", "qmux", "--example", "interop-server", "--features", "ws", "--", version],
		{
			cwd: ROOT,
			stdout: "pipe",
			stderr: "pipe",
		},
	);
	const stderr = new Response(server.stderr).text();

	try {
		// The first build can be cold when this test is run directly, so server
		// startup gets a larger bound than individual protocol operations.
		const url = await withTimeout("starting the Rust interop server", readLine(server.stdout), 60_000);
		expect(url).toStartWith("ws://127.0.0.1:");

		const session = new Session(url, {
			protocols: [PROTOCOL],
			versions: { [PROTOCOL]: version },
			requireProtocol: true,
			// Small receive windows force the Rust sender to observe and react to
			// MAX_DATA/MAX_STREAM_DATA updates during the large reverse transfer.
			config: {
				maxData: 64n * 1024n,
				maxStreamDataBidiLocal: 32n * 1024n,
				maxStreamDataBidiRemote: 32n * 1024n,
				maxStreamDataUni: 32n * 1024n,
			},
		});
		await withTimeout("opening the QMux session", session.ready);
		expect(session.protocol).toBe(PROTOCOL);

		const outgoing = await withTimeout(
			"creating the client unidirectional stream",
			session.createUnidirectionalStream(),
		);
		const outgoingWriter = outgoing.getWriter();
		await withTimeout("writing the client flow-control payload", outgoingWriter.write(payload(CLIENT_SEED)));
		await withTimeout("finishing the client unidirectional stream", outgoingWriter.close());

		const incomingReader = session.incomingUnidirectionalStreams.getReader();
		const incoming = await withTimeout("accepting the server unidirectional stream", incomingReader.read());
		incomingReader.releaseLock();
		expect(incoming.done).toBe(false);
		if (incoming.value === undefined) throw new Error("server unidirectional stream was missing");
		expectPayload(
			await withTimeout("reading the server flow-control payload", readAll(incoming.value)),
			SERVER_SEED,
		);

		const bidi = await withTimeout("creating the bidirectional stream", session.createBidirectionalStream());
		const bidiWriter = bidi.writable.getWriter();
		await withTimeout(
			"writing the bidirectional request",
			bidiWriter.write(new TextEncoder().encode(`ping:${version}`)),
		);
		await withTimeout("finishing the bidirectional request", bidiWriter.close());
		const bidiResponse = await withTimeout("reading the bidirectional response", readAll(bidi.readable));
		expect(new TextDecoder().decode(bidiResponse)).toBe(`pong:${version}`);

		if (version === "qmux-00") {
			expect(session.datagrams.maxDatagramSize).toBe(0);
		} else {
			expect(session.datagrams.maxDatagramSize).toBeGreaterThan(0);
			const datagramReader = session.datagrams.readable.getReader();
			const fromRust = await withTimeout("receiving the Rust datagram", datagramReader.read());
			datagramReader.releaseLock();
			expect(fromRust.done).toBe(false);
			expect(new TextDecoder().decode(fromRust.value)).toBe("rust-datagram");

			const datagramWriter = session.datagrams.writable.getWriter();
			await datagramWriter.write(new TextEncoder().encode("typescript-datagram"));
			datagramWriter.releaseLock();
		}

		session.close({ closeCode: 42, reason: "interop complete" });
		expect(await withTimeout("closing the TypeScript session", session.closed)).toEqual({
			closeCode: 42,
			reason: "interop complete",
		});

		const exitCode = await withTimeout("waiting for the Rust interop server", server.exited);
		if (exitCode !== 0) {
			throw new Error(`Rust interop server exited with ${exitCode}:\n${await stderr}`);
		}
	} catch (error) {
		server.kill();
		await server.exited;
		const diagnostics = await stderr;
		throw new Error(`${error instanceof Error ? error.message : String(error)}\n${diagnostics}`);
	}
}

test("TypeScript client interoperates with the Rust WebSocket server across supported drafts", async () => {
	for (const version of ["qmux-02", "qmux-01", "qmux-00"] as const) {
		await runVersion(version);
	}
}, 120_000);
