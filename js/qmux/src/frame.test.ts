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
