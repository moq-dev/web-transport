import { execSync } from "node:child_process";

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
	console.log(`⏭️  ${name}@${version} already published, skipping`);
	process.exit(0);
}

console.log(`📦 Building ${name}@${version}...`);
execSync("bun run build", { stdio: "inherit" });

console.log(`🚀 Publishing ${name}@${version}...`);
execSync("npm publish --access public --provenance", {
	stdio: "inherit",
	cwd: "dist",
});
