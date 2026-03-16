// ChatGPT made a script that rewrites package.json file to use the correct paths.
// The problem is that I want paths to reference `src` during development, but `dist` during release.
// It's not pretty but nothing in NPM is.

import { execSync } from "node:child_process";

console.log("📦 Building package...");
execSync("bun run build", { stdio: "inherit" });

console.log("🚀 Publishing...");
execSync("bunx npm publish --access=public", {
	stdio: "inherit",
	cwd: "dist",
});
