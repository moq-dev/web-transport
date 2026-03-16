// @ts-expect-error embed the certificate fingerprint using bundler
import fingerprintHex from "bundle-text:../dev/localhost.hex";

// Convert the hex to binary.
const fingerprint = [];
for (let c = 0; c < fingerprintHex.length - 1; c += 2) {
	fingerprint.push(parseInt(fingerprintHex.substring(c, c + 2), 16));
}

const params = new URLSearchParams(window.location.search);

const url = params.get("url") || "https://localhost:4443";
const datagram = params.get("datagram") || false;
const protocol = params.get("protocol") || null;

function log(msg) {
	const element = document.createElement("div");
	element.innerText = msg;

	document.body.appendChild(element);
}

async function run() {
	// Connect using the hex fingerprint in the cert folder.
	const options = {
		serverCertificateHashes: [
			{
				algorithm: "sha-256",
				value: new Uint8Array(fingerprint),
			},
		],
	};

	// Add protocols if specified via query parameter
	if (protocol) {
		options.protocols = [protocol];
		log(`requesting protocol: ${protocol}`);
	}

	const transport = new WebTransport(url, options);
	await transport.ready;

	log("connected");

	// Log the negotiated protocol
	if (transport.protocol) {
		log(`negotiated protocol: ${transport.protocol}`);
	} else if (protocol) {
		log("no protocol negotiated (server did not select one)");
	}

	let writer;
	let reader;

	if (!datagram) {
		// Create a bidirectional stream
		const stream = await transport.createBidirectionalStream();
		log("created stream");

		writer = stream.writable.getWriter();
		reader = stream.readable.getReader();
	} else {
		log("using datagram");

		// Create a datagram
		writer = transport.datagrams.writable.getWriter();
		reader = transport.datagrams.readable.getReader();
	}

	// Create a message
	const msg = "Hello, world!";
	const encoded = new TextEncoder().encode(msg);

	await writer.write(encoded);
	await writer.close();
	writer.releaseLock();

	log(`send: ${msg}`);

	// Read a message from it
	// TODO handle partial reads
	const { value } = await reader.read();

	const recv = new TextDecoder().decode(value);
	log(`recv: ${recv}`);

	transport.close();
	log("closed");
}

run();
