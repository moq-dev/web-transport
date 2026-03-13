import * as Stream from "./stream.ts";
import { VarInt } from "./varint.ts";

export type Version = "webtransport" | "qmux-00";

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

export interface Padding {
	type: "padding";
}

export interface Ping {
	type: "ping";
}

export type Any = Data | ResetStream | StopSending | ConnectionClose;

export function encode(frame: Any, version: Version = "webtransport"): Uint8Array {
	if (version === "webtransport") {
		return encodeWebTransport(frame);
	}
	return encodeQMux(frame);
}

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
	}
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

	// Flow control and other frames — ignore
	// MAX_DATA, MAX_STREAM_DATA, MAX_STREAMS, DATA_BLOCKED, etc.
	return null;
}
