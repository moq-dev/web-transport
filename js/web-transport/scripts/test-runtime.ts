import { spawn } from "node:child_process";
import { cp, mkdir, mkdtemp, readdir, rm, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";

const PACKAGE = resolve(import.meta.dir, "..");
const ROOT = resolve(PACKAGE, "../..");
const stage = await mkdtemp(join(tmpdir(), "web-transport-node-runtime-"));
const env = { ...process.env };
// Loading must use the artifact inside the packed package.
delete env.NAPI_RS_NATIVE_LIBRARY_PATH;
delete env.NAPI_RS_FORCE_WASI;

async function run(command: string[], cwd: string, timeoutMs = 15 * 60_000): Promise<void> {
	console.log(`> ${command.join(" ")}`);
	const child = spawn(command[0], command.slice(1), { cwd, env, stdio: "inherit", detached: true });
	let timedOut = false;
	const timer = setTimeout(() => {
		timedOut = true;
		// Kill the entire build/test process group, including any compiler children.
		try {
			if (child.pid !== undefined) process.kill(-child.pid, "SIGKILL");
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
		}
	}, timeoutMs);
	try {
		await new Promise<void>((resolve, reject) => {
			child.once("error", reject);
			child.once("close", (code, signal) => {
				if (timedOut) reject(new Error(`Timed out after ${timeoutMs}ms: ${command.join(" ")}`));
				else if (code !== 0) reject(new Error(`Command failed (${signal ?? code}): ${command.join(" ")}`));
				else resolve();
			});
		});
	} finally {
		clearTimeout(timer);
	}
}

try {
	const prepared = join(stage, "package");
	await cp(PACKAGE, prepared, {
		recursive: true,
		filter: (path) =>
			!["node_modules", "dist", "target", "artifacts"].includes(basename(path)) && !path.endsWith(".node"),
	});
	await run(
		[
			"bunx",
			"--no-install",
			"napi",
			"build",
			"--platform",
			"--release",
			"--manifest-path",
			join(ROOT, "rs/web-transport-node/Cargo.toml"),
			"--output-dir",
			prepared,
			"--js",
			"napi.cjs",
			"--dts",
			"napi.d.cts",
		],
		PACKAGE,
	);
	// prepack runs the same TypeScript build as publishing. Only its build
	// tools come from the checkout; the extracted package has no such link.
	await symlink(join(ROOT, "node_modules"), join(prepared, "node_modules"), "dir");
	await run(["bun", "pm", "pack", "--destination", stage], prepared);
	const archives = (await readdir(stage)).filter((name) => name.endsWith(".tgz"));
	if (archives.length !== 1) throw new Error(`Expected one packed package, found ${archives.length}`);
	const installed = join(stage, "node_modules/@moq/web-transport");
	await mkdir(installed, { recursive: true });
	await run(["tar", "-xzf", join(stage, archives[0]), "-C", installed, "--strip-components=1"], stage);
	if (!(await readdir(installed)).some((name) => name.endsWith(".node"))) {
		throw new Error("The packed package did not contain the native artifact");
	}
	await cp(join(PACKAGE, "tests/consumer.ts"), join(stage, "consumer.mts"));
	await run(
		[
			"bunx",
			"--no-install",
			"tsc",
			"--ignoreConfig",
			"--noEmit",
			"--strict",
			"--target",
			"esnext",
			"--module",
			"nodenext",
			"--lib",
			"esnext,dom,dom.iterable",
			"--types",
			"node",
			"--typeRoots",
			join(ROOT, "node_modules/@types"),
			join(stage, "consumer.mts"),
		],
		PACKAGE,
	);
	await cp(join(PACKAGE, "tests/runtime.mjs"), join(stage, "runtime.mjs"));
	const cert = join(stage, "cert.pem");
	const key = join(stage, "key.pem");
	await run(
		[
			"openssl",
			"req",
			"-x509",
			"-newkey",
			"ec",
			"-pkeyopt",
			"ec_paramgen_curve:prime256v1",
			"-keyout",
			key,
			"-out",
			cert,
			"-days",
			"1",
			"-nodes",
			"-subj",
			"/CN=localhost",
		],
		stage,
	);
	for (const mode of ["natural", "forced"]) {
		await run(["node", "--unhandled-rejections=strict", "runtime.mjs", mode, cert, key], stage, 30_000);
	}
} finally {
	await rm(stage, { recursive: true, force: true });
}
