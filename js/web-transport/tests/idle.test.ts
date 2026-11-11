import { expect, test } from "bun:test";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

test("idle timeout and keep-alive options apply to Node sessions", () => {
	const output = execFileSync("node", [fileURLToPath(new URL("./fixtures/idle.ts", import.meta.url))], {
		encoding: "utf8",
		timeout: 20_000,
		stdio: ["ignore", "pipe", "pipe"],
	});
	expect(output.trim()).toBe("idle");
}, 25_000);
