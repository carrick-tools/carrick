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

test("what ships: the entry point, the source, the sidecar and the templates", () => {
  for (const entry of ["bin", "src", "sidecar", "templates"]) {
    assert.ok((pkg["files"] as string[]).includes(entry), `files is missing ${entry}`);
  }
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
