import type { NapiRequest } from "../napi.js";
import Session from "./session.ts";

export class Request {
	#inner: NapiRequest;

	/** @internal */
	constructor(inner: NapiRequest) {
		this.#inner = inner;
	}

	get url(): Promise<string> {
		return this.#inner.url;
	}

	async ok(): Promise<Session> {
		const session = await this.#inner.ok();
		return new Session(session);
	}

	reject(status: number): Promise<void> {
		return this.#inner.reject(status);
	}
}
