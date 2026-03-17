// Script to build and package a workspace for distribution
// This creates a dist/ folder with the correct paths and dependencies for publishing
// Split from release.ts to allow building packages without publishing

import { copyFileSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

console.log("✍️  Rewriting package.json...");
const pkg = JSON.parse(readFileSync("package.json", "utf8"));

function rewritePath(p: string): string {
	return p.replace(/^\.\/src/, ".");
}

function rewriteExtension(p: string): string {
	return p.replace(/\.ts(x)?$/, ".js");
}

pkg.main &&= rewriteExtension(rewritePath(pkg.main));
pkg.types &&= rewritePath(pkg.types);

if (pkg.exports) {
	for (const key in pkg.exports) {
		const val = pkg.exports[key];
		if (typeof val === "string") {
			pkg.exports[key] = rewriteExtension(rewritePath(val));
		} else if (typeof val === "object") {
			for (const sub in val) {
				if (typeof val[sub] === "string") {
					val[sub] = rewriteExtension(rewritePath(val[sub]));
				}
			}
		}
	}
}

if (pkg.sideEffects) {
	pkg.sideEffects = pkg.sideEffects.map(rewriteExtension).map(rewritePath);
}

if (pkg.files) {
	pkg.files = pkg.files.map(rewritePath);
}

// Convert workspace dependencies to published versions
if (pkg.dependencies) {
	for (const [name, version] of Object.entries(pkg.dependencies)) {
		if (typeof version === "string" && version.startsWith("workspace:")) {
			// Read the actual version from the workspace package
			// Handle both scoped (@scope/name) and unscoped (name) packages
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
for (const file of ["README.md", "../../LICENSE-MIT", "../../LICENSE-APACHE"]) {
	const dest = join("dist", file.replace(/^\.\.\/+/g, ""));
	copyFileSync(file, dest);
	console.log(`📄 Copied ${file}`);
}

console.log("📦 Package built successfully in dist/");
