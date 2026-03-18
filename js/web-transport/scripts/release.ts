import { execSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";

// Read package.json to get name and version
const pkg = JSON.parse(await Bun.file("package.json").text());
const { name, version } = pkg;

// Check if this version is already published
let published = "0.0.0";
try {
	published = execSync(`npm view ${name} version`, {
		encoding: "utf8",
		stdio: ["pipe", "pipe", "pipe"],
	}).trim();
} catch {
	// Package not published yet
}

if (version === published) {
	console.log(`${name}@${version} already published, skipping`);
	process.exit(0);
}

// Move downloaded artifacts into the correct platform package directories
if (existsSync("artifacts")) {
	console.log("Moving artifacts...");
	execSync("bunx napi artifacts -d artifacts", { stdio: "inherit" });
}

// Publish each platform-specific package first
for (const dir of readdirSync("npm")) {
	const pkgPath = `npm/${dir}/package.json`;
	if (existsSync(pkgPath)) {
		console.log(`Publishing ${dir}...`);
		execSync("npm publish --access public", {
			stdio: "inherit",
			cwd: `npm/${dir}`,
		});
	}
}

// Publish the main package
console.log(`Publishing ${name}@${version}...`);
execSync("npm publish --access public", { stdio: "inherit" });
