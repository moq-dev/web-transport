#!/usr/bin/env node

// Example Node.js client using the WebTransport polyfill
// This demonstrates how to connect to a WebTransport server from Node.js

import { WebSocket } from "ws";
import WebTransportWs from "../src/index";

// Install the polyfill globally for Node.js
globalThis.WebTransport = WebTransportWs;
globalThis.WebSocket = WebSocket;

async function main() {
	const url = process.argv[2] || "http://localhost:3000";

	console.log(`Connecting to ${url}...`);

	// Create a new WebTransport connection
	const transport = new WebTransport(url);

	// Add error handling for closed promise
	transport.closed.then(
		(info) => console.log("Transport closed with info:", info),
		(error) => console.error("Transport closed with error:", error),
	);

	// Wait for the connection to be ready
	await transport.ready;
	console.log("✓ Connected successfully");

	// Example 1: Send data on a unidirectional stream
	console.log("\n--- Sending unidirectional stream ---");
	const sendStream = await transport.createUnidirectionalStream();
	const writer = sendStream.getWriter();

	const message = "Hello from Node.js client!";
	await writer.write(new TextEncoder().encode(message));
	await writer.close();
	console.log(`✓ Sent: "${message}"`);

	// Example 2: Create and use a bidirectional stream
	console.log("\n--- Creating bidirectional stream ---");
	const biStream = await transport.createBidirectionalStream();

	// Send data
	const biWriter = biStream.writable.getWriter();
	const request = "ping";
	await biWriter.write(new TextEncoder().encode(request));
	await biWriter.close();
	console.log(`✓ Sent: "${request}"`);

	// Read response
	const biReader = biStream.readable.getReader();
	const { value, done } = await biReader.read();
	if (!done && value) {
		const response = new TextDecoder().decode(value);
		console.log(`✓ Received: "${response}"`);
	}
	biReader.releaseLock();

	// Example 3: Listen for incoming streams
	console.log("\n--- Listening for incoming streams ---");

	// Handle incoming unidirectional streams
	const uni: ReadableStreamDefaultReader<ReadableStream<Uint8Array>> =
		transport.incomingUnidirectionalStreams.getReader();

	const { value: stream } = await uni.read();
	if (!stream) throw new Error("No stream received");

	const reader = stream.getReader();
	const { value: data } = await reader.read();
	if (!data) throw new Error("No data received");

	console.log(`✓ Received uni stream: "${new TextDecoder().decode(data)}"`);
	reader.releaseLock();
	uni.releaseLock();

	// Close the connection
	console.log("\n--- Closing connection ---");
	transport.close({
		closeCode: 0,
		reason: "Client finished",
	});

	// Wait for closed
	const closeInfo = await transport.closed;
	console.log(`✓ Connection closed: ${closeInfo.reason || "No reason"}`);
}

// Run the client
main().catch(console.error);
