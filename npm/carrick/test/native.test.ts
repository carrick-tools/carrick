// Finding the binary inside an npm install, including the shapes that go wrong.
//
// The install this has to survive is `npm install --ignore-scripts`: no
// lifecycle script runs, so whatever npm itself put on disk is all there is.
// The tests build that layout in a tempdir and resolve against it.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { createRequire } from "node:module";
import {
  PLATFORMS,
  binaryName,
  platformPackage,
  resolveNativeBinary,
  resolveSidecarDir,
  nativeEnv,
} from "../src/native.ts";

/** An install as npm leaves it: one platform package, no scripts run. */
function installedLayout(platform: string, arch: string, withBinary = true): string {
  // realpath: on macOS the temp directory is a symlink and require.resolve
  // answers with the resolved path, so the expected paths must be built from it.
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-install-")));
  const pkg = path.join(root, "node_modules", "@carrick-tools", `cli-${platform}-${arch}`);
  fs.mkdirSync(path.join(pkg, "bin"), { recursive: true });
  fs.writeFileSync(
    path.join(pkg, "package.json"),
    JSON.stringify({ name: platformPackage(platform, arch), version: "0.0.0" }),
  );
  if (withBinary) fs.writeFileSync(path.join(pkg, "bin", binaryName(platform)), "#!/bin/sh\n");
  return root;
}

/** Resolution as it happens from a file inside that install. */
function resolverFor(root: string) {
  const require = createRequire(path.join(root, "node_modules", "carrick", "src", "native.ts"));
  return (specifier: string) => require.resolve(specifier);
}

test("the platform package npm installed carries the binary", () => {
  const root = installedLayout("linux", "x64");
  const lookup = resolveNativeBinary({
    env: {},
    platform: "linux",
    arch: "x64",
    resolveManifest: resolverFor(root),
  });
  assert.equal(lookup.source, "platform_package");
  assert.equal(lookup.problem, null);
  assert.equal(
    lookup.binary,
    path.join(root, "node_modules", "@carrick-tools", "cli-linux-x64", "bin", "carrick"),
  );
});

test("on Windows the file is carrick.exe", () => {
  const root = installedLayout("win32", "x64");
  const lookup = resolveNativeBinary({
    env: {},
    platform: "win32",
    arch: "x64",
    resolveManifest: resolverFor(root),
  });
  assert.ok(lookup.binary?.endsWith("carrick.exe"), lookup.binary ?? "no binary");
});

test("a platform we publish for, not installed, names the package and the fix", () => {
  const root = installedLayout("linux", "x64");
  const lookup = resolveNativeBinary({
    env: {},
    platform: "darwin",
    arch: "arm64",
    resolveManifest: resolverFor(root),
  });
  assert.equal(lookup.binary, null);
  assert.match(lookup.problem ?? "", /@carrick-tools\/cli-darwin-arm64/);
  assert.match(lookup.problem ?? "", /npm install carrick/);
});

test("a platform we publish no binary for says which ones exist", () => {
  const root = installedLayout("linux", "x64");
  const lookup = resolveNativeBinary({
    env: {},
    platform: "freebsd",
    arch: "x64",
    resolveManifest: resolverFor(root),
  });
  assert.equal(lookup.binary, null);
  assert.match(lookup.problem ?? "", /publishes no binary for freebsd-x64/);
  assert.match(lookup.problem ?? "", /linux-x64/);
});

test("a platform package with no binary in it is a message, not a crash", () => {
  const root = installedLayout("linux", "x64", false);
  const lookup = resolveNativeBinary({
    env: {},
    platform: "linux",
    arch: "x64",
    resolveManifest: resolverFor(root),
  });
  assert.equal(lookup.binary, null);
  assert.match(lookup.problem ?? "", /holds no binary/);
});

test("CARRICK_NATIVE_BINARY wins, and is checked", () => {
  const found = resolveNativeBinary({
    env: { CARRICK_NATIVE_BINARY: "/build/carrick" },
    exists: (target) => target === "/build/carrick",
  });
  assert.deepEqual(found, { binary: "/build/carrick", problem: null, source: "env" });

  const missing = resolveNativeBinary({
    env: { CARRICK_NATIVE_BINARY: "/build/gone" },
    exists: () => false,
  });
  assert.equal(missing.binary, null);
  assert.match(missing.problem ?? "", /\/build\/gone/);
});

test("every platform in the list has a package name of the same shape", () => {
  for (const { platform, arch } of PLATFORMS) {
    assert.equal(platformPackage(platform, arch), `@carrick-tools/cli-${platform}-${arch}`);
  }
});

test("the sidecar directory is the bundled one unless the environment names another", () => {
  assert.equal(resolveSidecarDir({ env: { CARRICK_SIDECAR_DIR: "/elsewhere" } }), "/elsewhere");
  assert.equal(resolveSidecarDir({ env: {}, exists: () => false }), null);
  const bundled = resolveSidecarDir({ env: {}, exists: () => true });
  assert.ok(bundled?.endsWith(`${path.sep}sidecar`), bundled ?? "no directory");
});

test("the environment handed to the binary carries the sidecar and nothing else new", () => {
  const env = nativeEnv({ env: { PATH: "/usr/bin" }, exists: () => true });
  assert.equal(env["PATH"], "/usr/bin");
  assert.ok(env["CARRICK_SIDECAR_DIR"]);
  const without = nativeEnv({ env: { PATH: "/usr/bin" }, exists: () => false });
  assert.deepEqual(without, { PATH: "/usr/bin" });
});
