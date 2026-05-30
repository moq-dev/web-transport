import napi from "../napi.cjs";
import { Request } from "./request.ts";

const { NapiServer } = napi;

export class Server {
	#inner: InstanceType<typeof NapiServer>;

	private constructor(inner: InstanceType<typeof NapiServer>) {
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
