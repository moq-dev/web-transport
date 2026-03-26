import { NapiServer, type NapiRequest } from "../index.js";
import Session from "./session.ts";

/** A pending WebTransport session request from a client. */
export class Request {
	#inner: NapiRequest;

	/** @internal */
	constructor(inner: NapiRequest) {
		this.#inner = inner;
	}

	/** Get the URL of the CONNECT request. */
	get url(): Promise<string> {
		return this.#inner.url;
	}

	/** Accept the session with 200 OK, returning a W3C-compatible Session. */
	async ok(): Promise<Session> {
		const napi = await this.#inner.ok();
		return new Session(napi);
	}

	/** Reject the session with the given HTTP status code. */
	reject(status: number): Promise<void> {
		return this.#inner.reject(status);
	}
}

/** A WebTransport server that accepts incoming sessions. */
export class Server {
	#inner: NapiServer;

	private constructor(inner: NapiServer) {
		this.#inner = inner;
	}

	/** Create a server bound to the given address with the given TLS certificate. */
	static bind(addr: string, certPem: Buffer, keyPem: Buffer): Server {
		return new Server(NapiServer.bind(addr, certPem, keyPem));
	}

	/** Accept the next incoming WebTransport session request. */
	async accept(): Promise<Request | null> {
		const inner = await this.#inner.accept();
		if (!inner) return null;
		return new Request(inner);
	}
}
