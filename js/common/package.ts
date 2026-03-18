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

pkg.main &&= rewritePath(pkg.main, "js");
pkg.types &&= rewritePath(pkg.types, "d.ts");

if (pkg.exports) {
	for (const key in pkg.exports) {
		const val = pkg.exports[key];
		if (typeof val === "string") {
			pkg.exports[key] = {
				types: rewritePath(val, "d.ts"),
				default: rewritePath(val, "js"),
			};
		} else if (typeof val === "object") {
			for (const sub in val) {
				if (typeof val[sub] === "string") {
					val[sub] = rewritePath(val[sub], sub === "types" ? "d.ts" : "js");
				}
			}
		}
	}
}

if (pkg.sideEffects) {
	pkg.sideEffects = pkg.sideEffects.map((p: string) => rewritePath(p, "js"));
}

// Remove the files field; everything in dist/ should be published.
delete pkg.files;

if (pkg.bin) {
	if (typeof pkg.bin === "string") {
		pkg.bin = rewritePath(pkg.bin, "js");
	} else if (typeof pkg.bin === "object") {
		for (const key in pkg.bin) {
			pkg.bin[key] = rewritePath(pkg.bin[key], "js");
		}
	}
}

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
