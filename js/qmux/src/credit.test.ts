import { describe, expect, test } from "bun:test";
import { Credit } from "./credit.ts";

/** Settle a claim() to its value or its rejection error, without leaking an
 *  unhandled rejection. */
function settle<T>(p: Promise<T>): Promise<T | Error> {
	return p.then(
		(v) => v,
		(e) => e,
	);
}

describe("Credit basics still work", () => {
	test("claim() resolves and clamps to remaining credit", async () => {
		const credit = new Credit(10n);
		expect(await credit.claim(4n)).toBe(4n);
		expect(await credit.claim(100n)).toBe(6n);
	});

	test("zero-limit claim resolves to 0n even after close", async () => {
		const credit = new Credit(0n);
		credit.close(new Error("Connection closed: 0 "));
		expect(await credit.claim(0n)).toBe(0n);
	});
});

describe("Credit.close reason propagation", () => {
	test("claim() after close(reason) rejects with the exact reason instance", async () => {
		const credit = new Credit(0n);
		const reason = new Error("Connection closed: 42 bye");
		credit.close(reason);

		expect(await settle(credit.claim(1n))).toBe(reason);
	});

	test("a claim() pending at close(reason) rejects with the exact reason instance", async () => {
		const credit = new Credit(0n); // no credit available -> claim parks on a waiter
		const reason = new Error("Connection closed: 7 gone");

		const settled = settle(credit.claim(1n));
		credit.close(reason);
		expect(await settled).toBe(reason);
	});

	test("close() with no reason rejects with a generic Error('closed')", async () => {
		const credit = new Credit(0n);
		const pending = settle(credit.claim(1n));
		credit.close();

		const pendingErr = await pending;
		expect(pendingErr).toBeInstanceOf(Error);
		expect((pendingErr as Error).message).toBe("closed");

		// A claim() made *after* a reason-less close also gets the generic Error.
		const afterErr = await settle(credit.claim(1n));
		expect(afterErr).toBeInstanceOf(Error);
		expect((afterErr as Error).message).toBe("closed");
	});

	test("first close(reason) wins; later close() calls do not overwrite it", async () => {
		const credit = new Credit(0n);
		const first = new Error("Connection closed: 1 first");
		credit.close(first);
		credit.close(new Error("Connection closed: 2 second"));
		credit.close(); // reason-less close after a reasoned one

		expect(await settle(credit.claim(1n))).toBe(first);
	});
});

describe("Credit.increaseMax ignores stale limits", () => {
	test("a lower or equal limit neither reduces credit nor wakes a waiter", async () => {
		const credit = new Credit(2n);
		expect(credit.tryClaim(2n)).toBe(2n);

		let woke = false;
		const pending = credit.claim(1n).then((v) => {
			woke = true;
			return v;
		});

		credit.increaseMax(1n);
		credit.increaseMax(2n);
		await Promise.resolve();
		expect(credit.max).toBe(2n);
		expect(woke).toBe(false);

		credit.increaseMax(3n);
		expect(await pending).toBe(1n);
		expect(credit.max).toBe(3n);
	});
});

describe("Credit.claim cancellation", () => {
	test("an already-aborted signal rejects with its reason, even with credit free", async () => {
		const credit = new Credit(10n);
		const reason = new Error("cancelled");
		expect(await settle(credit.claim(1n, AbortSignal.abort(reason)))).toBe(reason);
		expect(credit.available).toBe(10n);
	});

	test("aborting a parked claim rejects it and leaves later credit for others", async () => {
		const credit = new Credit(0n);
		const controller = new AbortController();
		const reason = new Error("cancelled");
		const cancelled = settle(credit.claim(5n, controller.signal));
		const other = credit.claim(5n);

		controller.abort(reason);
		expect(await cancelled).toBe(reason);

		credit.increaseMax(5n);
		expect(await other).toBe(5n);
		expect(credit.available).toBe(0n);
	});

	test("a claim satisfied before the abort keeps its credit", async () => {
		const credit = new Credit(0n);
		const controller = new AbortController();
		const claim = credit.claim(3n, controller.signal);
		credit.increaseMax(3n);
		expect(await claim).toBe(3n);
		controller.abort();
		expect(credit.used).toBe(3n);
	});
});
