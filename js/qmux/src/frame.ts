import * as Stream from "./stream.ts";
import { VarInt } from "./varint.ts";

export type Version = "webtransport" | "qmux-00" | "qmux-01";

/** Maximum size of a single QMux frame on the wire. */
export const MAX_FRAME_SIZE = 16384;

/** Maximum payload per STREAM frame, accounting for frame overhead (24 bytes). */
export const MAX_FRAME_PAYLOAD = MAX_FRAME_SIZE - 24;

export interface Data {
	type: "stream";
	id: Stream.Id;
	data: Uint8Array;
	fin: boolean;
}

export interface ResetStream {
	type: "reset_stream";
	id: Stream.Id;
	code: VarInt;
}

export interface StopSending {
	type: "stop_sending";
	id: Stream.Id;
	code: VarInt;
}

export interface ConnectionClose {
	type: "connection_close";
	code: VarInt;
	reason: string;
}

export interface MaxData {
	type: "max_data";
	max: bigint;
}

export interface MaxStreamData {
	type: "max_stream_data";
	id: Stream.Id;
	max: bigint;
}

export interface MaxStreamsBidi {
	type: "max_streams_bidi";
	max: bigint;
}

export interface MaxStreamsUni {
	type: "max_streams_uni";
	max: bigint;
}

export interface DataBlocked {
	type: "data_blocked";
	limit: bigint;
}

export interface StreamDataBlocked {
	type: "stream_data_blocked";
	id: Stream.Id;
	limit: bigint;
}

export interface StreamsBlockedBidi {
	type: "streams_blocked_bidi";
	limit: bigint;
}

export interface StreamsBlockedUni {
	type: "streams_blocked_uni";
	limit: bigint;
}

export interface TransportParameters {
	type: "transport_parameters";
	params: TransportParams;
}

export interface TransportParams {
	maxIdleTimeout: bigint;
	initialMaxData: bigint;
	initialMaxStreamDataBidiLocal: bigint;
	initialMaxStreamDataBidiRemote: bigint;
	initialMaxStreamDataUni: bigint;
	initialMaxStreamsBidi: bigint;
	initialMaxStreamsUni: bigint;
	maxRecordSize: bigint;
}

/** Default max_record_size per draft-01. */
export const DEFAULT_MAX_RECORD_SIZE = 16382n;

export const DEFAULT_TRANSPORT_PARAMS: TransportParams = {
	maxIdleTimeout: 0n,
	initialMaxData: 0n,
	initialMaxStreamDataBidiLocal: 0n,
	initialMaxStreamDataBidiRemote: 0n,
	initialMaxStreamDataUni: 0n,
	initialMaxStreamsBidi: 0n,
	initialMaxStreamsUni: 0n,
	maxRecordSize: DEFAULT_MAX_RECORD_SIZE,
};

export const RECOMMENDED_TRANSPORT_PARAMS: TransportParams = {
	maxIdleTimeout: 30_000n,
	initialMaxData: 1_048_576n,
	initialMaxStreamDataBidiLocal: 262_144n,
	initialMaxStreamDataBidiRemote: 262_144n,
	initialMaxStreamDataUni: 262_144n,
	initialMaxStreamsBidi: 100n,
	initialMaxStreamsUni: 100n,
	maxRecordSize: DEFAULT_MAX_RECORD_SIZE,
};

export interface Padding {
	type: "padding";
}

/** QX_PING frame (draft-01) */
export interface PingRequest {
	type: "ping_request";
	sequence: bigint;
}

export interface PingResponse {
	type: "ping_response";
	sequence: bigint;
}

export type Any =
	| Data
	| ResetStream
	| StopSending
	| ConnectionClose
	| MaxData
	| MaxStreamData
	| MaxStreamsBidi
	| MaxStreamsUni
	| DataBlocked
	| StreamDataBlocked
	| StreamsBlockedBidi
	| StreamsBlockedUni
	| TransportParameters
	| PingRequest
	| PingResponse;

export function encode(frame: Any, version: Version = "webtransport"): Uint8Array {
	if (version === "webtransport") {
		return encodeWebTransport(frame);
	}
	return encodeQMux(frame);
}

/** Returns true if the version uses QMux framing (draft-00 or later). */
export function isQmux(version: Version): boolean {
	return version === "qmux-00" || version === "qmux-01";
}

// QX_PING frame type constants (draft-01)
const QX_PING_REQUEST = 0x348c67529ef8c7bdn;
const QX_PING_RESPONSE = 0x348c67529ef8c7ben;
// max_record_size transport parameter ID
const MAX_RECORD_SIZE_ID = 0x0571c59429cd0845n;

function encodeWebTransport(frame: Any): Uint8Array {
	switch (frame.type) {
		case "stream": {
			let buffer = new Uint8Array(new ArrayBuffer(1 + 8 + frame.data.length), 0, 1);

			buffer[0] = frame.fin ? 0x09 : 0x08;
			buffer = frame.id.value.encode(buffer);

			buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + frame.data.length);
			buffer.set(frame.data, buffer.byteLength - frame.data.length);

			return buffer;
		}

		case "reset_stream": {
			let buffer = new Uint8Array(new ArrayBuffer(1 + 8 + 8), 0, 1);

			buffer[0] = 0x04;
			buffer = frame.id.value.encode(buffer);
			buffer = frame.code.encode(buffer);
			return buffer;
		}

		case "stop_sending": {
			let buffer = new Uint8Array(new ArrayBuffer(1 + 8 + 8), 0, 1);

			buffer[0] = 0x05;
			buffer = frame.id.value.encode(buffer);
			buffer = frame.code.encode(buffer);
			return buffer;
		}

		case "connection_close": {
			const body = new TextEncoder().encode(frame.reason);
			let buffer = new Uint8Array(new ArrayBuffer(1 + 8 + body.length), 0, 1);

			buffer[0] = 0x1d;
			buffer = frame.code.encode(buffer);

			buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + body.length);
			buffer.set(body, buffer.byteLength - body.length);

			return buffer;
		}

		default:
			throw new Error("flow control frames are not supported in WebTransport version");
	}
}

function encodeQMux(frame: Any): Uint8Array {
	switch (frame.type) {
		case "stream": {
			// Always set LEN bit (0x02), type = 0x0a | fin_bit
			const frameType = VarInt.from(0x0a | (frame.fin ? 0x01 : 0x00));
			const lengthVi = VarInt.from(frame.data.length);

			const maxSize = 8 + 8 + 8 + frame.data.length;
			let buffer = new Uint8Array(new ArrayBuffer(maxSize), 0, 0);

			buffer = frameType.encode(buffer);
			buffer = frame.id.value.encode(buffer);
			buffer = lengthVi.encode(buffer);

			buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + frame.data.length);
			buffer.set(frame.data, buffer.byteLength - frame.data.length);

			return buffer;
		}

		case "reset_stream": {
			const frameType = VarInt.from(0x04);
			const finalSize = VarInt.from(0);

			let buffer = new Uint8Array(new ArrayBuffer(8 + 8 + 8 + 8), 0, 0);

			buffer = frameType.encode(buffer);
			buffer = frame.id.value.encode(buffer);
			buffer = frame.code.encode(buffer);
			buffer = finalSize.encode(buffer);
			return buffer;
		}

		case "stop_sending": {
			const frameType = VarInt.from(0x05);

			let buffer = new Uint8Array(new ArrayBuffer(8 + 8 + 8), 0, 0);

			buffer = frameType.encode(buffer);
			buffer = frame.id.value.encode(buffer);
			buffer = frame.code.encode(buffer);
			return buffer;
		}

		case "connection_close": {
			// APPLICATION_CLOSE (0x1d)
			const frameType = VarInt.from(0x1d);
			const causingFrameType = VarInt.from(0);
			const body = new TextEncoder().encode(frame.reason);
			const reasonLength = VarInt.from(body.length);

			let buffer = new Uint8Array(new ArrayBuffer(8 + 8 + 8 + 8 + body.length), 0, 0);

			buffer = frameType.encode(buffer);
			buffer = frame.code.encode(buffer);
			buffer = causingFrameType.encode(buffer);
			buffer = reasonLength.encode(buffer);

			buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + body.length);
			buffer.set(body, buffer.byteLength - body.length);

			return buffer;
		}

		case "max_data": {
			let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
			buffer = VarInt.from(0x10).encode(buffer);
			buffer = VarInt.from(frame.max).encode(buffer);
			return buffer;
		}

		case "max_stream_data": {
			let buffer = new Uint8Array(new ArrayBuffer(24), 0, 0);
			buffer = VarInt.from(0x11).encode(buffer);
			buffer = frame.id.value.encode(buffer);
			buffer = VarInt.from(frame.max).encode(buffer);
			return buffer;
		}

		case "max_streams_bidi": {
			let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
			buffer = VarInt.from(0x12).encode(buffer);
			buffer = VarInt.from(frame.max).encode(buffer);
			return buffer;
		}

		case "max_streams_uni": {
			let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
			buffer = VarInt.from(0x13).encode(buffer);
			buffer = VarInt.from(frame.max).encode(buffer);
			return buffer;
		}

		case "data_blocked": {
			let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
			buffer = VarInt.from(0x14).encode(buffer);
			buffer = VarInt.from(frame.limit).encode(buffer);
			return buffer;
		}

		case "stream_data_blocked": {
			let buffer = new Uint8Array(new ArrayBuffer(24), 0, 0);
			buffer = VarInt.from(0x15).encode(buffer);
			buffer = frame.id.value.encode(buffer);
			buffer = VarInt.from(frame.limit).encode(buffer);
			return buffer;
		}

		case "streams_blocked_bidi": {
			let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
			buffer = VarInt.from(0x16).encode(buffer);
			buffer = VarInt.from(frame.limit).encode(buffer);
			return buffer;
		}

		case "streams_blocked_uni": {
			let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
			buffer = VarInt.from(0x17).encode(buffer);
			buffer = VarInt.from(frame.limit).encode(buffer);
			return buffer;
		}

		case "transport_parameters": {
			const payload = encodeTransportParams(frame.params);
			let buffer = new Uint8Array(new ArrayBuffer(8 + 8 + payload.byteLength), 0, 0);
			buffer = VarInt.from(0x3f5153300d0a0d0an).encode(buffer);
			buffer = VarInt.from(payload.byteLength).encode(buffer);
			buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + payload.byteLength);
			buffer.set(payload, buffer.byteLength - payload.byteLength);
			return buffer;
		}

		case "ping_request": {
			let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
			buffer = VarInt.from(QX_PING_REQUEST).encode(buffer);
			buffer = VarInt.from(frame.sequence).encode(buffer);
			return buffer;
		}

		case "ping_response": {
			let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
			buffer = VarInt.from(QX_PING_RESPONSE).encode(buffer);
			buffer = VarInt.from(frame.sequence).encode(buffer);
			return buffer;
		}
	}
}

function encodeTransportParams(params: TransportParams): Uint8Array<ArrayBuffer> {
	// Each param: id(varint) + length(varint) + value(varint)
	// Max 8 params * 24 bytes each
	let buffer = new Uint8Array(new ArrayBuffer(192), 0, 0);

	function writeParam(buf: Uint8Array<ArrayBuffer>, id: number | bigint, value: bigint): Uint8Array<ArrayBuffer> {
		if (value === 0n) return buf;
		const valVi = VarInt.from(value);
		buf = VarInt.from(id).encode(buf);
		buf = VarInt.from(valVi.size()).encode(buf);
		buf = valVi.encode(buf);
		return buf;
	}

	buffer = writeParam(buffer, 0x01, params.maxIdleTimeout);
	buffer = writeParam(buffer, 0x04, params.initialMaxData);
	buffer = writeParam(buffer, 0x05, params.initialMaxStreamDataBidiLocal);
	buffer = writeParam(buffer, 0x06, params.initialMaxStreamDataBidiRemote);
	buffer = writeParam(buffer, 0x07, params.initialMaxStreamDataUni);
	buffer = writeParam(buffer, 0x08, params.initialMaxStreamsBidi);
	buffer = writeParam(buffer, 0x09, params.initialMaxStreamsUni);
	buffer = writeParam(buffer, MAX_RECORD_SIZE_ID, params.maxRecordSize);

	return buffer;
}

function decodeTransportParams(buffer: Uint8Array): TransportParams {
	const params = { ...DEFAULT_TRANSPORT_PARAMS };
	let v: VarInt;

	while (buffer.byteLength > 0) {
		[v, buffer] = VarInt.decode(buffer);
		const id = v.value;

		[v, buffer] = VarInt.decode(buffer);
		const len = Number(v.value);

		if (buffer.byteLength < len) {
			throw new Error("transport parameter truncated");
		}

		const paramData = buffer.slice(0, len);
		buffer = buffer.slice(len);

		if (paramData.byteLength < 1) {
			continue; // Empty param, skip
		}

		let paramValue: bigint;
		[v] = VarInt.decode(paramData);
		paramValue = v.value;

		switch (id) {
			case 0x01n:
				params.maxIdleTimeout = paramValue;
				break;
			case 0x04n:
				params.initialMaxData = paramValue;
				break;
			case 0x05n:
				params.initialMaxStreamDataBidiLocal = paramValue;
				break;
			case 0x06n:
				params.initialMaxStreamDataBidiRemote = paramValue;
				break;
			case 0x07n:
				params.initialMaxStreamDataUni = paramValue;
				break;
			case 0x08n:
				params.initialMaxStreamsBidi = paramValue;
				break;
			case 0x09n:
				params.initialMaxStreamsUni = paramValue;
				break;
			case MAX_RECORD_SIZE_ID:
				params.maxRecordSize = paramValue;
				break;
			// Unknown params: skip
		}
	}

	return params;
}

export function decode(buffer: Uint8Array, version: Version = "webtransport"): Any | null {
	if (buffer.length === 0) {
		throw new Error("Invalid frame: empty buffer");
	}

	if (version === "webtransport") {
		return decodeWebTransport(buffer);
	}
	return decodeQMux(buffer);
}

/** Decode all frames from a QMux Record payload (draft-01).
 * A record contains one or more frames concatenated together.
 */
export function decodeRecord(buffer: Uint8Array): Any[] {
	const frames: Any[] = [];
	while (buffer.byteLength > 0) {
		const result = decodeQMuxOne(buffer);
		if (result === null) break;
		const [frame, remaining] = result;
		if (frame !== null) {
			frames.push(frame);
		}
		buffer = remaining;
	}
	return frames;
}

/** Encode a frame wrapped in a QMux Record (Size + Frames) for draft-01. */
export function encodeRecord(frameBytes: Uint8Array): Uint8Array {
	const sizeVi = VarInt.from(frameBytes.byteLength);
	const totalSize = sizeVi.size() + frameBytes.byteLength;
	let buffer = new Uint8Array(new ArrayBuffer(totalSize), 0, 0);
	buffer = sizeVi.encode(buffer);
	buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + frameBytes.byteLength);
	buffer.set(frameBytes, buffer.byteLength - frameBytes.byteLength);
	return buffer;
}

function decodeWebTransport(buffer: Uint8Array): Any {
	const frameType = buffer[0];
	buffer = buffer.slice(1);

	let v: VarInt;

	if (frameType === 0x04) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);

		[v, buffer] = VarInt.decode(buffer);
		const code = v;

		return { type: "reset_stream", id, code };
	}

	if (frameType === 0x05) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);

		[v, buffer] = VarInt.decode(buffer);
		const code = v;

		return { type: "stop_sending", id, code };
	}

	if (frameType === 0x1d) {
		[v, buffer] = VarInt.decode(buffer);
		const code = v;

		const reason = new TextDecoder().decode(buffer);

		return { type: "connection_close", code, reason };
	}

	if (frameType === 0x08 || frameType === 0x09) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);

		return {
			type: "stream",
			id,
			data: buffer,
			fin: frameType === 0x09,
		};
	}

	throw new Error(`Invalid frame type: ${frameType}`);
}

function decodeQMux(buffer: Uint8Array): Any | null {
	let v: VarInt;

	[v, buffer] = VarInt.decode(buffer);
	const frameType = v.value;

	// STREAM frames: 0x08-0x0f
	if (frameType >= 0x08n && frameType <= 0x0fn) {
		const hasOff = (frameType & 0x04n) !== 0n;
		const hasLen = (frameType & 0x02n) !== 0n;
		const hasFin = (frameType & 0x01n) !== 0n;

		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);

		// Skip offset if present
		if (hasOff) {
			[v, buffer] = VarInt.decode(buffer);
		}

		let data: Uint8Array;
		if (hasLen) {
			[v, buffer] = VarInt.decode(buffer);
			const len = Number(v.value);
			data = buffer.slice(0, len);
			buffer = buffer.slice(len);
		} else {
			data = buffer;
		}

		return { type: "stream", id, data, fin: hasFin };
	}

	// RESET_STREAM
	if (frameType === 0x04n) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);

		[v, buffer] = VarInt.decode(buffer);
		const code = v;

		// Skip final_size
		[v, buffer] = VarInt.decode(buffer);

		return { type: "reset_stream", id, code };
	}

	// STOP_SENDING
	if (frameType === 0x05n) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);

		[v, buffer] = VarInt.decode(buffer);
		const code = v;

		return { type: "stop_sending", id, code };
	}

	// CONNECTION_CLOSE / APPLICATION_CLOSE
	if (frameType === 0x1cn || frameType === 0x1dn) {
		[v, buffer] = VarInt.decode(buffer);
		const code = v;

		// Skip frame_type field
		[v, buffer] = VarInt.decode(buffer);

		// reason_length + reason
		[v, buffer] = VarInt.decode(buffer);
		const reasonLen = Number(v.value);
		const reason = new TextDecoder().decode(buffer.slice(0, reasonLen));

		return { type: "connection_close", code, reason };
	}

	// MAX_DATA
	if (frameType === 0x10n) {
		[v, buffer] = VarInt.decode(buffer);
		return { type: "max_data", max: v.value };
	}

	// MAX_STREAM_DATA
	if (frameType === 0x11n) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);
		[v, buffer] = VarInt.decode(buffer);
		return { type: "max_stream_data", id, max: v.value };
	}

	// MAX_STREAMS (bidi)
	if (frameType === 0x12n) {
		[v, buffer] = VarInt.decode(buffer);
		return { type: "max_streams_bidi", max: v.value };
	}

	// MAX_STREAMS (uni)
	if (frameType === 0x13n) {
		[v, buffer] = VarInt.decode(buffer);
		return { type: "max_streams_uni", max: v.value };
	}

	// DATA_BLOCKED
	if (frameType === 0x14n) {
		[v, buffer] = VarInt.decode(buffer);
		return { type: "data_blocked", limit: v.value };
	}

	// STREAM_DATA_BLOCKED
	if (frameType === 0x15n) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);
		[v, buffer] = VarInt.decode(buffer);
		return { type: "stream_data_blocked", id, limit: v.value };
	}

	// STREAMS_BLOCKED (bidi)
	if (frameType === 0x16n) {
		[v, buffer] = VarInt.decode(buffer);
		return { type: "streams_blocked_bidi", limit: v.value };
	}

	// STREAMS_BLOCKED (uni)
	if (frameType === 0x17n) {
		[v, buffer] = VarInt.decode(buffer);
		return { type: "streams_blocked_uni", limit: v.value };
	}

	// QX_TRANSPORT_PARAMETERS
	if (frameType === 0x3f5153300d0a0d0an) {
		[v, buffer] = VarInt.decode(buffer);
		const len = Number(v.value);
		if (buffer.byteLength < len) {
			throw new Error("transport parameters frame truncated");
		}
		const payload = buffer.slice(0, len);
		const params = decodeTransportParams(payload);
		return { type: "transport_parameters", params };
	}

	// QX_PING request
	if (frameType === QX_PING_REQUEST) {
		[v, buffer] = VarInt.decode(buffer);
		return { type: "ping_request", sequence: v.value };
	}

	// QX_PING response
	if (frameType === QX_PING_RESPONSE) {
		[v, buffer] = VarInt.decode(buffer);
		return { type: "ping_response", sequence: v.value };
	}

	// DATAGRAM without length
	if (frameType === 0x30n) {
		return null;
	}

	// DATAGRAM with length
	if (frameType === 0x31n) {
		[v, buffer] = VarInt.decode(buffer);
		return null;
	}

	// Unknown frame type
	return null;
}

/** Decode a single QMux frame, returning the frame and remaining buffer.
 * Returns null if the buffer is empty.
 */
function decodeQMuxOne(buffer: Uint8Array): [Any | null, Uint8Array] | null {
	if (buffer.byteLength === 0) return null;

	let v: VarInt;
	[v, buffer] = VarInt.decode(buffer);
	const frameType = v.value;

	// PADDING
	if (frameType === 0x00n) {
		return [null, buffer];
	}

	// STREAM frames: 0x08-0x0f
	if (frameType >= 0x08n && frameType <= 0x0fn) {
		const hasOff = (frameType & 0x04n) !== 0n;
		const hasLen = (frameType & 0x02n) !== 0n;
		const hasFin = (frameType & 0x01n) !== 0n;

		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);

		if (hasOff) {
			[v, buffer] = VarInt.decode(buffer);
		}

		let data: Uint8Array;
		if (hasLen) {
			[v, buffer] = VarInt.decode(buffer);
			const len = Number(v.value);
			data = buffer.slice(0, len);
			buffer = buffer.slice(len);
		} else {
			data = buffer;
			buffer = buffer.slice(buffer.byteLength);
		}

		return [{ type: "stream", id, data, fin: hasFin }, buffer];
	}

	// RESET_STREAM
	if (frameType === 0x04n) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);
		[v, buffer] = VarInt.decode(buffer);
		const code = v;
		[v, buffer] = VarInt.decode(buffer); // final_size
		return [{ type: "reset_stream", id, code }, buffer];
	}

	// STOP_SENDING
	if (frameType === 0x05n) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);
		[v, buffer] = VarInt.decode(buffer);
		const code = v;
		return [{ type: "stop_sending", id, code }, buffer];
	}

	// CONNECTION_CLOSE / APPLICATION_CLOSE
	if (frameType === 0x1cn || frameType === 0x1dn) {
		[v, buffer] = VarInt.decode(buffer);
		const code = v;
		[v, buffer] = VarInt.decode(buffer); // frame_type
		[v, buffer] = VarInt.decode(buffer);
		const reasonLen = Number(v.value);
		const reason = new TextDecoder().decode(buffer.slice(0, reasonLen));
		buffer = buffer.slice(reasonLen);
		return [{ type: "connection_close", code, reason }, buffer];
	}

	// MAX_DATA
	if (frameType === 0x10n) {
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "max_data", max: v.value }, buffer];
	}

	// MAX_STREAM_DATA
	if (frameType === 0x11n) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "max_stream_data", id, max: v.value }, buffer];
	}

	// MAX_STREAMS (bidi)
	if (frameType === 0x12n) {
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "max_streams_bidi", max: v.value }, buffer];
	}

	// MAX_STREAMS (uni)
	if (frameType === 0x13n) {
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "max_streams_uni", max: v.value }, buffer];
	}

	// DATA_BLOCKED
	if (frameType === 0x14n) {
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "data_blocked", limit: v.value }, buffer];
	}

	// STREAM_DATA_BLOCKED
	if (frameType === 0x15n) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "stream_data_blocked", id, limit: v.value }, buffer];
	}

	// STREAMS_BLOCKED (bidi)
	if (frameType === 0x16n) {
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "streams_blocked_bidi", limit: v.value }, buffer];
	}

	// STREAMS_BLOCKED (uni)
	if (frameType === 0x17n) {
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "streams_blocked_uni", limit: v.value }, buffer];
	}

	// QX_TRANSPORT_PARAMETERS
	if (frameType === 0x3f5153300d0a0d0an) {
		[v, buffer] = VarInt.decode(buffer);
		const len = Number(v.value);
		const payload = buffer.slice(0, len);
		buffer = buffer.slice(len);
		const params = decodeTransportParams(payload);
		return [{ type: "transport_parameters", params }, buffer];
	}

	// QX_PING request
	if (frameType === QX_PING_REQUEST) {
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "ping_request", sequence: v.value }, buffer];
	}

	// QX_PING response
	if (frameType === QX_PING_RESPONSE) {
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "ping_response", sequence: v.value }, buffer];
	}

	// DATAGRAM without length
	if (frameType === 0x30n) {
		return [null, buffer.slice(buffer.byteLength)];
	}

	// DATAGRAM with length
	if (frameType === 0x31n) {
		[v, buffer] = VarInt.decode(buffer);
		const len = Number(v.value);
		buffer = buffer.slice(len);
		return [null, buffer];
	}

	// Unknown: skip remaining (can't delimit)
	return [null, buffer.slice(buffer.byteLength)];
}
