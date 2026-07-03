import { describe, expect, test } from "bun:test";
import * as Frame from "./frame.ts";
import * as Stream from "./stream.ts";
import { VarInt } from "./varint.ts";

describe("QMux01 record framing", () => {
	test("decodeRecord parses multiple frames concatenated in one record body", () => {
		// Build the record body the way the wire layer hands it to us:
		// frames concatenated, no leading size varint (the transport already stripped it).
		const id = new Stream.Id(VarInt.from(0n));
		const frames: Frame.Any[] = [
			{ type: "stream", id, data: new Uint8Array([1, 2, 3, 4, 5]), fin: false },
			{ type: "ping_request", sequence: 42n },
			{ type: "max_data", max: 1024n },
		];

		const parts = frames.map((f) => Frame.encode(f, "qmux-01"));
		const totalLen = parts.reduce((sum, p) => sum + p.byteLength, 0);
		const body = new Uint8Array(totalLen);
		let offset = 0;
		for (const p of parts) {
			body.set(p, offset);
			offset += p.byteLength;
		}

		const decoded = Frame.decodeRecord(body);
		expect(decoded.length).toBe(3);

		const [first, second, third] = decoded;
		expect(first.type).toBe("stream");
		if (first.type === "stream") {
			expect(Array.from(first.data)).toEqual([1, 2, 3, 4, 5]);
			expect(first.fin).toBe(false);
		}
		expect(second.type).toBe("ping_request");
		if (second.type === "ping_request") {
			expect(second.sequence).toBe(42n);
		}
		expect(third.type).toBe("max_data");
		if (third.type === "max_data") {
			expect(third.max).toBe(1024n);
		}
	});

	test("application_protocols parameter is rejected (WebSocket negotiates via subprotocol)", () => {
		// A QX_TRANSPORT_PARAMETERS frame carrying only application_protocols
		// (id 0x3d4f9c2a8b1e6075, len 0). WebSocket has its own ALPN, so this
		// parameter must never appear and decoding must throw.
		const bytes = (...vals: number[]) => new Uint8Array(vals);
		const record = bytes(
			// frame type 0x3f5153300d0a0d0a — 8-byte varint, so the high tag bits
			// make the first wire byte 0xff (the "\xffQS0\r\n\r\n" magic).
			0xff,
			0x51,
			0x53,
			0x30,
			0x0d,
			0x0a,
			0x0d,
			0x0a,
			// length = 9 (param id 8 bytes + len 1 byte)
			0x09,
			// param id 0x3d4f9c2a8b1e6075 — 8-byte varint, first wire byte 0xfd.
			0xfd,
			0x4f,
			0x9c,
			0x2a,
			0x8b,
			0x1e,
			0x60,
			0x75,
			// param length = 0
			0x00,
		);
		expect(() => Frame.decode(record, "qmux-00")).toThrow();
	});

	test("ping_request and ping_response round-trip preserves the sequence number", () => {
		const req: Frame.Any = { type: "ping_request", sequence: 0xdeadbeefn };
		const reqBytes = Frame.encode(req, "qmux-01");
		const reqDecoded = Frame.decodeRecord(reqBytes);
		expect(reqDecoded.length).toBe(1);
		expect(reqDecoded[0]).toEqual({ type: "ping_request", sequence: 0xdeadbeefn });

		const resp: Frame.Any = { type: "ping_response", sequence: 0xdeadbeefn };
		const respBytes = Frame.encode(resp, "qmux-01");
		const respDecoded = Frame.decodeRecord(respBytes);
		expect(respDecoded.length).toBe(1);
		expect(respDecoded[0]).toEqual({ type: "ping_response", sequence: 0xdeadbeefn });
	});

	test("datagram round-trips via the length-prefixed (0x31) form", () => {
		const frame: Frame.Any = { type: "datagram", data: new Uint8Array([1, 2, 3, 4]) };
		const bytes = Frame.encode(frame, "qmux-01");
		// 0x31, len=4, then payload.
		expect(bytes[0]).toBe(0x31);
		const decoded = Frame.decodeRecord(bytes);
		expect(decoded.length).toBe(1);
		expect(decoded[0].type).toBe("datagram");
		if (decoded[0].type === "datagram") {
			expect(Array.from(decoded[0].data)).toEqual([1, 2, 3, 4]);
		}
	});

	test("datagram no-length (0x30) form decodes (payload runs to end of record)", () => {
		// We never emit 0x30, but must accept it: 0x30 followed by the payload.
		const wire = new Uint8Array([0x30, 0x68, 0x69]);
		const decoded = Frame.decode(wire, "qmux-00");
		expect(decoded?.type).toBe("datagram");
		if (decoded?.type === "datagram") {
			expect(Array.from(decoded.data)).toEqual([0x68, 0x69]);
		}
	});

	test("max_datagram_frame_size transport parameter round-trips", () => {
		const params: Frame.Any = {
			type: "transport_parameters",
			params: { ...Frame.DEFAULT_TRANSPORT_PARAMS, initialMaxData: 1024n, maxDatagramFrameSize: 1201n },
		};
		const decoded = Frame.decodeRecord(Frame.encode(params, "qmux-01"));
		expect(decoded.length).toBe(1);
		if (decoded[0].type === "transport_parameters") {
			expect(decoded[0].params.maxDatagramFrameSize).toBe(1201n);
			expect(decoded[0].params.initialMaxData).toBe(1024n);
		}
	});

	test("decodeTransportParams seeds maxRecordSize with the draft-01 default when the parameter is omitted", () => {
		// Empty params buffer → all values default; maxRecordSize must be 16382, not 0.
		const params: Frame.TransportParameters = {
			type: "transport_parameters",
			params: {
				maxIdleTimeout: 0n,
				initialMaxData: 0n,
				initialMaxStreamDataBidiLocal: 0n,
				initialMaxStreamDataBidiRemote: 0n,
				initialMaxStreamDataUni: 0n,
				initialMaxStreamsBidi: 0n,
				initialMaxStreamsUni: 0n,
				maxDatagramFrameSize: 0n,
				// Deliberately set to 0 — exercises the encoder's "skip-if-zero" + decoder's default seeding.
				maxRecordSize: 0n,
			},
		};
		const bytes = Frame.encode(params, "qmux-01");
		const decoded = Frame.decodeRecord(bytes);
		expect(decoded.length).toBe(1);
		const got = decoded[0];
		expect(got.type).toBe("transport_parameters");
		if (got.type === "transport_parameters") {
			expect(got.params.maxRecordSize).toBe(Frame.DEFAULT_MAX_RECORD_SIZE);
		}
	});
});
