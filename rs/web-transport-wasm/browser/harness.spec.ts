import { expect, test } from "@playwright/test";

test("every WASM harness check passes in Chromium", async ({ page }, testInfo) => {
	const errors: string[] = [];
	page.on("console", (message) => console.log(`browser ${message.type()}: ${message.text()}`));
	page.on("pageerror", (error) => {
		errors.push(error.message);
		console.error(`browser page error: ${error.message}`);
	});
	page.on("requestfailed", (request) =>
		console.error(`request failed: ${request.url()}: ${request.failure()?.errorText}`),
	);

	await page.goto("/");
	await page.waitForFunction(() => (window as unknown as { harness?: { done: boolean } }).harness?.done);
	const result = await page.evaluate(() => {
		const harness = (window as unknown as { harness: { passed: number; failed: number; rows: unknown[] } }).harness;
		return { ...harness, status: document.getElementById("status")?.textContent };
	});
	await testInfo.attach("harness-results", {
		body: JSON.stringify(result, null, 2),
		contentType: "application/json",
	});
	console.log(JSON.stringify(result, null, 2));
	expect(errors, "browser page errors").toEqual([]);
	expect(result.rows.length, "harness must execute assertions").toBeGreaterThan(0);
	expect(result.failed, result.status ?? "failed harness checks").toBe(0);
	expect(result.passed).toBe(result.rows.length);
	for (const row of result.rows as { name: string; ok: boolean; detail: string }[]) {
		expect(row.ok, `${row.name}: ${row.detail}`).toBe(true);
	}
});
