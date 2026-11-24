// Script to build and package a workspace for distribution
// This creates a dist/ folder with the correct paths and dependencies for publishing
// Split from release.ts to allow building packages without publishing

import { copyFileSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { publint } from "publint";
import { formatMessage } from "publint/utils";

console.log("✍️  Rewriting package.json...");
const pkg = JSON.parse(readFileSync("package.json", "utf8"));

function rewritePath(p: string, ext: string): string {
	return p.replace(/^\.\/src/, ".").replace(/\.ts(x)?$/, `.${ext}`);
}

pkg.types = rewritePath(pkg.types, "d.ts");

// Each export is a source path, published as compiled JS with its declarations.
for (const key in pkg.exports) {
	const path: string = pkg.exports[key];
	pkg.exports[key] = { types: rewritePath(path, "d.ts"), default: rewritePath(path, "js") };
}

// Remove the files field; everything in dist/ should be published.
delete pkg.files;

// Convert workspace dependencies to published versions
if (pkg.dependencies) {
	for (const [name, version] of Object.entries(pkg.dependencies)) {
		if (typeof version === "string" && version.startsWith("workspace:")) {
			const packageDir = name.includes("/") ? name.split("/")[1] : name;
			const workspacePkgPath = `../${packageDir}/package.json`;
			const workspacePkg = JSON.parse(readFileSync(workspacePkgPath, "utf8"));
			pkg.dependencies[name] = `^${workspacePkg.version}`;
			console.log(`🔗 Converted ${name}: ${version} → ^${workspacePkg.version}`);
		}
	}
}

pkg.devDependencies = undefined;
pkg.scripts = undefined;

// Write the rewritten package.json
writeFileSync("dist/package.json", JSON.stringify(pkg, null, 2));

// Copy static files
console.log("📄 Copying README.md...");
copyFileSync("README.md", join("dist", "README.md"));

// Lint the package to catch publishing issues
console.log("🔍 Running publint...");
const { messages, pkg: lintPkg } = await publint({
	pkgDir: resolve("dist"),
	level: "warning",
	pack: false,
});

if (messages.length > 0) {
	for (const message of messages) {
		console.error(formatMessage(message, lintPkg));
	}
	process.exit(1);
}

console.log("📦 Package built successfully in dist/");
