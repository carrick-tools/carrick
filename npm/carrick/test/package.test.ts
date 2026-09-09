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
  assert.equal(pkg["exports"]["./templates"], "./dist/templates.js");
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
