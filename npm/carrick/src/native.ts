// Finding the scanner binary, and the sidecar it needs, inside an npm install.
//
// The Rust binary cannot ship in this package: npm would put five platforms'
// worth of it on every machine. It ships in `@carrick-tools/cli-<platform>`
// packages listed as `optionalDependencies`, so npm installs exactly the one
// this machine can run and skips the rest. Nothing here runs at install time —
// no postinstall, no download — which is why `npm install --ignore-scripts`
// still produces a working `carrick`.
//
// The sidecar goes the other way round: it is TypeScript, it is the same on
// every platform, and it needs `ts-morph` and `zod` resolvable from where it
// sits. So it ships in THIS package, beside `node_modules/`, and the binary is
// told where it is with CARRICK_SIDECAR_DIR (src/main.rs reads it first).

import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

/** Where the binary for a platform is published. */
export function platformPackage(
  platform: string = process.platform,
  arch: string = process.arch,
): string {
  return `@carrick-tools/cli-${platform}-${arch}`;
}

/** Every platform this package publishes a binary for. */
export const PLATFORMS: Array<{ platform: string; arch: string }> = [
  { platform: "darwin", arch: "arm64" },
  { platform: "darwin", arch: "x64" },
  { platform: "linux", arch: "x64" },
  { platform: "linux", arch: "arm64" },
  { platform: "win32", arch: "x64" },
];

/** The file name inside a platform package. */
export function binaryName(platform: string = process.platform): string {
  return platform === "win32" ? "carrick.exe" : "carrick";
}

/** This package's own root, whatever the cwd is. */
export function packageRoot(): string {
  return path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
}

export type NativeLookup = {
  /** Absolute path to the scanner binary, or null when nothing resolved. */
  binary: string | null;
  /** Why there is none, ready to print. Null when there is one. */
  problem: string | null;
  /** How it was found, for the log and for the tests. */
  source: "env" | "platform_package" | "none";
};

export type ResolveOptions = {
  env?: NodeJS.ProcessEnv;
  platform?: string;
  arch?: string;
  /** Injectable for tests: resolves a package's `package.json` to a path. */
  resolveManifest?: (specifier: string) => string;
  exists?: (target: string) => boolean;
};

function defaultResolveManifest(specifier: string): string {
  // Resolved from this file, so the lookup follows the install that owns this
  // copy of the package rather than the caller's cwd.
  return createRequire(import.meta.url).resolve(specifier);
}

function defaultExists(target: string): boolean {
  try {
    return fs.existsSync(target);
  } catch {
    return false;
  }
}

/**
 * The scanner binary this machine should run.
 *
 * CARRICK_NATIVE_BINARY wins, so a developer can point the shipped CLI at a
 * `cargo build` and the smokes can pin one. Otherwise the platform package
 * that npm installed carries it. A missing platform package is a message, not
 * a crash: the caller decides whether that is fatal (running a scan) or merely
 * silent (a hook that must never fail an edit).
 */
export function resolveNativeBinary(options: ResolveOptions = {}): NativeLookup {
  const env = options.env ?? process.env;
  const exists = options.exists ?? defaultExists;
  const override = env["CARRICK_NATIVE_BINARY"];
  if (override) {
    if (exists(override)) {
      return { binary: override, problem: null, source: "env" };
    }
    return {
      binary: null,
      source: "env",
      problem: `CARRICK_NATIVE_BINARY is set to ${override}, which is not a file on this machine.`,
    };
  }

  const platform = options.platform ?? process.platform;
  const arch = options.arch ?? process.arch;
  const specifier = platformPackage(platform, arch);
  const resolveManifest = options.resolveManifest ?? defaultResolveManifest;
  let manifest: string;
  try {
    manifest = resolveManifest(`${specifier}/package.json`);
  } catch {
    const supported = PLATFORMS.some(
      (entry) => entry.platform === platform && entry.arch === arch,
    );
    return {
      binary: null,
      source: "none",
      problem: supported
        ? `The carrick binary for ${platform}-${arch} is not installed. It ships as ${specifier}, an optional dependency of this package. Reinstall with 'npm install carrick', or set CARRICK_NATIVE_BINARY to a binary you built.`
        : `Carrick publishes no binary for ${platform}-${arch}. Supported: ${PLATFORMS.map((entry) => `${entry.platform}-${entry.arch}`).join(", ")}. Set CARRICK_NATIVE_BINARY to a binary you built to use it anyway.`,
    };
  }

  const binary = path.join(path.dirname(manifest), "bin", binaryName(platform));
  if (!exists(binary)) {
    return {
      binary: null,
      source: "none",
      problem: `${specifier} is installed but holds no binary at ${binary}. Reinstall it, or set CARRICK_NATIVE_BINARY.`,
    };
  }
  return { binary, problem: null, source: "platform_package" };
}

/**
 * The sidecar directory to hand the binary, or null when this checkout has
 * none built. Null is not an error here: the binary's own discovery covers a
 * source checkout, and the read-only commands never start a sidecar at all.
 */
export function resolveSidecarDir(options: ResolveOptions = {}): string | null {
  const env = options.env ?? process.env;
  const exists = options.exists ?? defaultExists;
  const configured = env["CARRICK_SIDECAR_DIR"];
  if (configured) return configured;
  const bundled = path.join(packageRoot(), "sidecar");
  return exists(path.join(bundled, "dist", "src", "index.js")) ? bundled : null;
}

/**
 * The environment to run the binary with: this package's sidecar, and nothing
 * else changed.
 */
export function nativeEnv(options: ResolveOptions = {}): NodeJS.ProcessEnv {
  const env = { ...(options.env ?? process.env) };
  const sidecar = resolveSidecarDir(options);
  if (sidecar) env["CARRICK_SIDECAR_DIR"] = sidecar;
  return env;
}
