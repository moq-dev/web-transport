/** Tracks used/max credit for flow control.
 *
 * Mirrors the Rust `Credit` struct. Callers can synchronously try to claim
 * credit, or await until credit becomes available. Calling `close()` causes
 * pending and future `claim()` calls to reject.
 */
export class Credit {
	#used: bigint;
	#max: bigint;
	#closed = false;
	#waiters: Array<{ resolve: () => void; reject: (err: Error) => void }> = [];

	constructor(max: bigint) {
		this.#used = 0n;
		this.#max = max;
	}

	/** Try to claim up to `limit` units. Returns amount claimed (0n if none available). */
	tryClaim(limit: bigint): bigint {
		const available = this.#max - this.#used;
		if (available <= 0n) return 0n;
		const claimed = limit < available ? limit : available;
		this.#used += claimed;
		return claimed;
	}

	/** Claim up to `limit` units, waiting until credit is available.
	 *  Rejects if the credit has been closed. */
	async claim(limit: bigint): Promise<bigint> {
		while (true) {
			if (this.#closed) throw new Error("closed");

			const claimed = this.tryClaim(limit);
			if (claimed > 0n) return claimed;

			await new Promise<void>((resolve, reject) => {
				this.#waiters.push({ resolve, reject });
			});
		}
	}

	/** Return previously claimed credit (for rollback). */
	release(amount: bigint): void {
		this.#used = this.#used > amount ? this.#used - amount : 0n;
		this.#wake();
	}

	/** Increase the max. Returns false if new_max < current max. */
	increaseMax(newMax: bigint): boolean {
		if (newMax < this.#max) return false;
		if (newMax === this.#max) return true;
		this.#max = newMax;
		this.#wake();
		return true;
	}

	/** Close the credit, rejecting all pending and future `claim()` calls. */
	close(): void {
		this.#closed = true;
		const waiters = this.#waiters;
		this.#waiters = [];
		const err = new Error("closed");
		for (const { reject } of waiters) reject(err);
	}

	/** Get current available credit (max - used). */
	get available(): bigint {
		const avail = this.#max - this.#used;
		return avail > 0n ? avail : 0n;
	}

	/** Get the current max value. */
	get max(): bigint {
		return this.#max;
	}

	/** Get the current used value. */
	get used(): bigint {
		return this.#used;
	}

	#wake(): void {
		const waiters = this.#waiters;
		this.#waiters = [];
		for (const { resolve } of waiters) resolve();
	}
}
