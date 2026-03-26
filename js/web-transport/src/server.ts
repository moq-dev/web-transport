import { NapiServer } from "../napi.js";
import { Request } from "./request.ts";

export class Server {
	#inner: NapiServer;

	private constructor(inner: NapiServer) {
		this.#inner = inner;
	}

	static bind(addr: string, certPem: Buffer, keyPem: Buffer): Server {
		return new Server(NapiServer.bind(addr, certPem, keyPem));
	}

	async accept(): Promise<Request | null> {
		const inner = await this.#inner.accept();
		if (!inner) return null;
		return new Request(inner);
	}

	close(): void {
		this.#inner.close();
	}
}
