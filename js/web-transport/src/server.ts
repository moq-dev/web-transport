import napi from "../napi.cjs";
import { Request } from "./request.ts";
import type { TransportOptions } from "./session.ts";

/** Options for {@link Server.bind}. */
export type ServerOptions = TransportOptions;

const { NapiServer } = napi;

export class Server {
	#inner: InstanceType<typeof NapiServer>;

	private constructor(inner: InstanceType<typeof NapiServer>) {
		this.#inner = inner;
	}

	/** Bind a server to `addr`, serving the PEM-encoded certificate chain and private key. */
	static bind(addr: string, certPem: Buffer, keyPem: Buffer, options?: ServerOptions): Server {
		return new Server(
			NapiServer.bindWithOptions(addr, certPem, keyPem, {
				idleTimeout: options?.idleTimeout,
				keepAliveInterval: options?.keepAliveInterval,
			}),
		);
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
