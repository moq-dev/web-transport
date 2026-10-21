import { execFileSync } from "node:child_process";
import path from "node:path";
import { defineConfig } from "@playwright/test";

const root = path.resolve(import.meta.dirname, "../../..");
const target: string = JSON.parse(
	execFileSync("cargo", ["metadata", "--format-version", "1", "--no-deps"], { cwd: root, encoding: "utf8" }),
).target_directory;

export default defineConfig({
	testDir: import.meta.dirname,
	outputDir: path.join(target, "browser-results/tests"),
	reporter: [["list"], ["html", { outputFolder: path.join(target, "browser-results/report"), open: "never" }]],
	workers: 1,
	retries: 0,
	forbidOnly: true,
	timeout: 60_000,
	globalTimeout: 90_000,
	use: {
		browserName: "chromium",
		baseURL: "http://127.0.0.1:8080",
		trace: "retain-on-failure",
		screenshot: "only-on-failure",
	},
	webServer: {
		command: "./dev/serve-harness 8080",
		cwd: root,
		url: "http://127.0.0.1:8080/index.html",
		reuseExistingServer: false,
		timeout: 15_000,
		gracefulShutdown: { signal: "SIGTERM", timeout: 2_000 },
	},
});
