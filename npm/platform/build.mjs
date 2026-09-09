#!/usr/bin/env node
// Build one `@carrick-tools/cli-<platform>-<arch>` package around a built binary.
//
// One package per platform, listed as optional dependencies of `carrick`, is
// how a Rust binary reaches an npm install without a postinstall download:
// `os` and `cpu` make npm install exactly the one this machine can run and
// skip the rest, and skipping happens with no script running, so
// `--ignore-scripts` changes nothing.
//
// None of these declares `bin`. Only `carrick` does: a second package
// declaring the same command name would race it for the symlink, and which one
// won would depend on install order.
//
// Usage (release.yml, once per matrix entry):
//   node npm/platform/build.mjs \
//     --platform linux --arch x64 --version 0.3.48 \
//     --binary target/x86_64-unknown-linux-gnu/release/carrick \
//     --out dist/platform

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.join(here, "..", "..");

function options(argv) {
  const parsed = {};
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index];
    const value = argv[index + 1];
    if (!key?.startsWith("--") || value === undefined) {
      throw new Error(`expected --name value pairs, found ${argv.slice(index).join(" ")}`);
    }
    parsed[key.slice(2)] = value;
  }
  for (const required of ["platform", "arch", "version", "binary", "out"]) {
    if (!parsed[required]) throw new Error(`--${required} is required`);
  }
  return parsed;
}

export function manifest({ platform, arch, version }) {
  return {
    name: `@carrick-tools/cli-${platform}-${arch}`,
    version,
    description: `The carrick scanner binary for ${platform}-${arch}. Installed automatically as an optional dependency of the carrick package; there is no reason to depend on it directly.`,
    homepage: "https://carrick.tools",
    repository: {
      type: "git",
      url: "git+https://github.com/carrick-tools/carrick.git",
      directory: "npm/platform",
    },
    license: "SEE LICENSE IN LICENSE.md",
    os: [platform],
    cpu: [arch],
    files: ["bin", "LICENSE.md"],
    publishConfig: { access: "public", provenance: true },
  };
}

export function build({ platform, arch, version, binary, out }) {
  if (!fs.existsSync(binary)) throw new Error(`no binary at ${binary}`);
  const target = path.join(out, `${platform}-${arch}`);
  fs.rmSync(target, { recursive: true, force: true });
  fs.mkdirSync(path.join(target, "bin"), { recursive: true });
  const name = platform === "win32" ? "carrick.exe" : "carrick";
  fs.copyFileSync(binary, path.join(target, "bin", name));
  // The mode survives `npm pack` but not every checkout it came from, so it is
  // set here rather than assumed.
  if (platform !== "win32") fs.chmodSync(path.join(target, "bin", name), 0o755);
  fs.writeFileSync(
    path.join(target, "package.json"),
    `${JSON.stringify(manifest({ platform, arch, version }), null, 2)}\n`,
  );
  fs.copyFileSync(path.join(repoRoot, "LICENSE.md"), path.join(target, "LICENSE.md"));
  return target;
}

// `file://${argv[1]}` is not this module's URL on Windows, where argv[1] is
// `D:\a\...` and the URL is `file:///D:/a/...`. That comparison was false on
// the win32 leg of the release, so this script did nothing and exited 0 — the
// step passed, the package was never written, and the failure only surfaced
// two steps later as a missing directory (release 0.3.52).
if (pathToFileURL(process.argv[1] ?? "").href === import.meta.url) {
  try {
    const parsed = options(process.argv.slice(2));
    const target = build(parsed);
    process.stdout.write(`${manifest(parsed).name} built in ${target}\n`);
  } catch (error) {
    process.stderr.write(`carrick platform package: ${error.message}\n`);
    process.exit(1);
  }
}
