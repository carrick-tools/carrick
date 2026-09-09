// What the published package promises, asserted against the manifest.
//
// Every one of these has a failure mode that only shows up on a user's machine
// after a publish: a platform package pinned to the wrong version 404s, a
// missing `sidecar` in `files` ships a scanner that types nothing, a `bin` on a
// platform package races the main one for the command name.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { PLATFORMS, platformPackage } from "../src/native.ts";
import { manifest } from "../../platform/build.mjs";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = path.resolve(packageRoot, "..", "..");

function readJson(target: string): Record<string, any> {
  return JSON.parse(fs.readFileSync(target, "utf8"));
}

const pkg = readJson(path.join(packageRoot, "package.json"));

test("the package is named carrick and its command is the shim", () => {
  assert.equal(pkg["name"], "carrick");
  assert.deepEqual(pkg["bin"], { carrick: "bin/carrick.mjs" });
  const entry = fs.readFileSync(path.join(packageRoot, "bin", "carrick.mjs"), "utf8");
  assert.ok(entry.startsWith("#!/usr/bin/env node\n"));
});

test("every platform this package can resolve is an optional dependency at this version", () => {
  const optional = pkg["optionalDependencies"] as Record<string, string>;
  const expected = PLATFORMS.map(({ platform, arch }) => platformPackage(platform, arch)).sort();
  assert.deepEqual(Object.keys(optional).sort(), expected);
  for (const [name, range] of Object.entries(optional)) {
    assert.equal(
      range,
      pkg["version"],
      `${name} is pinned to ${range}, not ${pkg["version"]}: a release publishes them together, so a range that is not the exact version resolves to a package that does not exist yet`,
    );
  }
});

test("the lockfile agrees with the manifest about the version and the five pins", () => {
  // release-please bumps both, through `extra-files`. When it stops doing so
  // — a renamed field, a jsonpath that no longer matches, a pin added without
  // a config entry — the release publishes a package whose lockfile describes
  // a different version, and the first thing anyone sees is a failed publish.
  //
  // Only the manifest-level fields are compared. The `node_modules/...`
  // entries hold whatever the registry held when the lock was last written,
  // which for a version that is not published yet is a stub with no version
  // at all; the workflows regenerate the lock before `npm ci` for exactly
  // that reason.
  const lock = readJson(path.join(packageRoot, "package-lock.json"));
  assert.equal(lock["version"], pkg["version"], "package-lock.json version");
  assert.equal(lock["packages"][""]["version"], pkg["version"], "lock root package version");
  assert.deepEqual(
    lock["packages"][""]["optionalDependencies"],
    pkg["optionalDependencies"],
    "the lockfile's root optionalDependencies must be the manifest's, pin for pin",
  );
});

test("the version tracks the scanner's, because the Action installs it by that number", () => {
  const cargo = fs.readFileSync(path.join(repoRoot, "Cargo.toml"), "utf8");
  const version = /^version\s*=\s*"([^"]+)"/m.exec(cargo)?.[1];
  assert.equal(
    pkg["version"],
    version,
    "npm/carrick/package.json and Cargo.toml must carry the same version — release-please bumps both, and action.yml installs carrick@<the Cargo version>",
  );
});

test("what ships: the entry point, the emit, the sidecar and the templates", () => {
  for (const entry of ["bin", "dist", "sidecar", "templates"]) {
    assert.ok((pkg["files"] as string[]).includes(entry), `files is missing ${entry}`);
  }
});

test("the entry point can be run as a command, because a hook may name it", () => {
  // `carrick init` writes this path into a settings file when `carrick` does
  // not resolve on PATH (#837), and a settings hook is a line a shell runs.
  const entry = path.join(packageRoot, "bin", "carrick.mjs");
  assert.match(fs.readFileSync(entry, "utf8"), /^#!\/usr\/bin\/env node\n/);
  assert.ok(fs.statSync(entry).mode & 0o111, "bin/carrick.mjs is not executable");
});

test("the entry point imports the emit, never the TypeScript", () => {
  // Node refuses to strip types for any file under node_modules, so a `.ts`
  // import here works in this checkout and throws on every installed copy.
  const entry = fs.readFileSync(path.join(packageRoot, "bin", "carrick.mjs"), "utf8");
  const imports = [...entry.matchAll(/import\("([^"]+)"\)/g)].map((match) => match[1]);
  const relative = imports.filter((specifier) => specifier?.startsWith("."));
  assert.ok(relative.length > 0, "the entry point imports nothing of its own");
  for (const specifier of relative) {
    assert.match(specifier ?? "", /^\.\.\/dist\//, `${specifier} is not in the published emit`);
  }
  const hookTable = /const HOOKS = \{([^}]+)\}/.exec(entry)?.[1] ?? "";
  assert.doesNotMatch(hookTable, /\.ts"/);
});

test("the templates are importable by the tool that has to render the same bytes", () => {
  // The hosted scaffold tool depends on this package at a pinned version and
  // renders from here. Without an export map it would have to reach into
  // dist/ by path, which is not a contract anyone should rely on.
  //
  // And it is a TypeScript consumer, so the export has to carry its types: a
  // subpath with no `types` condition is TS7016 under any strict config, and
  // the consumer's only way out is to hand-write a declaration of this
  // module's surface — a second copy of the thing the shared template exists
  // to remove (#845).
  const templates = pkg["exports"]["./templates"] as Record<string, string>;
  assert.equal(templates["default"], "./dist/templates.js");
  assert.equal(templates["types"], "./dist/templates.d.ts");
  assert.equal(
    Object.keys(templates)[0],
    "types",
    "conditions resolve in order, so `types` has to come first",
  );
  // Read as text: the build config carries comments, so it is JSONC.
  const build = fs.readFileSync(path.join(packageRoot, "tsconfig.build.json"), "utf8");
  assert.match(
    build,
    /"declaration":\s*true/,
    "the file the types condition names has to be emitted",
  );
  assert.ok(fs.existsSync(path.join(packageRoot, "src", "templates.ts")));
});

test("the host manifests name this CLI's commands, and travel with the package", () => {
  // `--plugin-dir` has to name a directory an npm install actually has, so the
  // manifests are copied in at prepack; the repo's copy is the source.
  assert.ok((pkg["files"] as string[]).includes("plugin"));
  const hooks = readJson(path.join(repoRoot, "plugin", "hooks", "hooks.json"));
  const commands = Object.values(hooks["hooks"]).flatMap((groups: any) =>
    groups.flatMap((group: any) => group.hooks.map((entry: any) => entry.command)),
  );
  assert.deepEqual(commands.sort(), ["carrick hook post-edit", "carrick hook session-start"]);
  const lsp = readJson(path.join(repoRoot, "plugin", ".lsp.json"));
  assert.equal(lsp["carrick"]["command"], "carrick");
  assert.deepEqual(lsp["carrick"]["args"], ["lsp", "--stdio", "--hooks-installed"]);
});

test("the node floor is the sidecar's floor", () => {
  assert.equal(pkg["engines"]["node"], ">=24");
  const sidecar = readJson(path.join(repoRoot, "src", "sidecar", "package.json"));
  assert.match(sidecar["engines"]["node"], /24/);
});

test("the sidecar's dependencies are declared here, since nothing installs inside it", () => {
  const sidecar = readJson(path.join(repoRoot, "src", "sidecar", "package.json"));
  for (const name of Object.keys(sidecar["dependencies"] ?? {})) {
    assert.ok(
      name in (pkg["dependencies"] ?? {}),
      `${name} is a sidecar dependency and is not declared by npm/carrick/package.json`,
    );
  }
});

test("the tools the sidecar spawns are dependencies too, not just the ones it imports", () => {
  // `pnpm` and `tsc` are run as processes, so no import scan finds them, and
  // the release tarball only ever had them because its `npm ci` installed the
  // sidecar's devDependencies. Here they have to be real dependencies.
  const sidecar = readJson(path.join(repoRoot, "src", "sidecar", "package.json"));
  const dev = sidecar["devDependencies"] ?? {};
  assert.equal(pkg["dependencies"]["pnpm"], dev["pnpm"], "pnpm must be pinned to the version the sidecar vendors");
  assert.ok(pkg["dependencies"]["typescript"], "typescript is spawned as tsc and imported by the capture");
});

test("a platform package declares no command of its own", () => {
  for (const { platform, arch } of PLATFORMS) {
    const built = manifest({ platform, arch, version: pkg["version"] });
    assert.equal(built.name, platformPackage(platform, arch));
    assert.ok(!("bin" in built), `${built.name} declares bin, which would race carrick for the name`);
    assert.deepEqual(built.os, [platform]);
    assert.deepEqual(built.cpu, [arch]);
  }
});

test("no lifecycle script runs on install, so --ignore-scripts changes nothing", () => {
  const scripts = (pkg["scripts"] ?? {}) as Record<string, string>;
  for (const name of ["preinstall", "install", "postinstall"]) {
    assert.ok(!(name in scripts), `${name} would not run under --ignore-scripts`);
  }
});

test("the platform builder writes a package when run as a command", async () => {
  // Imported, `build()` is covered by the manifest test above. Run as a
  // command it goes through the entry guard, and a guard that is false writes
  // nothing and still exits 0 — which is how a release leg passed this step
  // with no package to show for it.
  const { execFileSync } = await import("node:child_process");
  const os = await import("node:os");
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-platform-"));
  const binary = path.join(root, "carrick");
  fs.writeFileSync(binary, "#!/bin/sh\n");
  execFileSync(
    process.execPath,
    [
      path.join(repoRoot, "npm", "platform", "build.mjs"),
      "--platform", "linux",
      "--arch", "x64",
      "--version", "9.9.9",
      "--binary", binary,
      "--out", path.join(root, "out"),
    ],
    { encoding: "utf8" },
  );
  const built = path.join(root, "out", "linux-x64");
  assert.ok(fs.existsSync(path.join(built, "package.json")), "no package.json was written");
  assert.ok(fs.existsSync(path.join(built, "bin", "carrick")), "no binary was written");
  assert.equal(readJson(path.join(built, "package.json"))["version"], "9.9.9");
});
