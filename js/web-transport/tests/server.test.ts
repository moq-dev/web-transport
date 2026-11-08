import { expect, test } from "bun:test";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

test("close stops a pending accept loop without blocking the JavaScript thread", () => {
	// The timeout lives in the parent process: a synchronous native deadlock also blocks JS timers.
	const output = execFileSync("node", [fileURLToPath(new URL("./fixtures/server-close.ts", import.meta.url))], {
		encoding: "utf8",
		timeout: 5_000,
		stdio: ["ignore", "pipe", "pipe"],
	});
	expect(output.trim()).toBe("closed");
}, 10_000);
