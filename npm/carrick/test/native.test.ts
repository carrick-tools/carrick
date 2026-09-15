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
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import {
  PLATFORMS,
  binaryName,
  overrideLine,
  packageRoot,
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

test("CARRICK_BIN wins, and is checked", () => {
  const found = resolveNativeBinary({
    env: { CARRICK_BIN: "/build/carrick" },
    exists: (target) => target === "/build/carrick",
  });
  assert.deepEqual(found, { binary: "/build/carrick", problem: null, source: "env" });

  const missing = resolveNativeBinary({
    env: { CARRICK_BIN: "/build/gone" },
    exists: () => false,
  });
  assert.equal(missing.binary, null);
  assert.match(missing.problem ?? "", /CARRICK_BIN is set to \/build\/gone/);
});

test("an overridden binary is named with its version; an installed one is not", () => {
  const overridden = { binary: "/build/carrick", problem: null, source: "env" as const };
  assert.equal(
    overrideLine(overridden, () => "carrick 0.3.99"),
    "carrick: running /build/carrick (carrick 0.3.99), set by CARRICK_BIN",
  );
  assert.equal(
    overrideLine(overridden, () => null),
    "carrick: running /build/carrick (version unknown), set by CARRICK_BIN",
  );
  const installed = { binary: "/x/bin/carrick", problem: null, source: "platform_package" as const };
  assert.equal(overrideLine(installed, () => "carrick 0.3.99"), null);
});

// carrick#1100: `index`, `status` and the other scanner commands resolved the
// installed binary and never read CARRICK_BIN, so a run meant for a local build
// silently ran whatever npm had installed.
test(
  "the scanner commands run the binary CARRICK_BIN names, and say which one",
  { skip: process.platform === "win32" ? "the stand-in binary is a POSIX script" : false },
  () => {
    const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-bin-")));
    const calls = path.join(dir, "calls.log");
    const binary = path.join(dir, "carrick");
    const script = [
      "#!/bin/sh",
      `echo "$*" >> "${calls}"`,
      'if [ "$1" = "--version" ]; then echo "carrick 0.0.0-local"; exit 0; fi',
      "echo answered",
      "",
    ];
    fs.writeFileSync(binary, script.join("\n"));
    fs.chmodSync(binary, 0o755);

    const run = spawnSync(
      process.execPath,
      [path.join(packageRoot(), "bin", "carrick.mjs"), "status", "--json"],
      { encoding: "utf8", env: { ...process.env, CARRICK_BIN: binary } },
    );

    assert.equal(run.status, 0, run.stderr);
    assert.equal(run.stdout.trim(), "answered", "stdout carries the binary's answer and nothing else");
    assert.ok(
      run.stderr.includes(`carrick: running ${binary} (carrick 0.0.0-local), set by CARRICK_BIN`),
      run.stderr,
    );
    assert.deepEqual(fs.readFileSync(calls, "utf8").trim().split("\n"), ["--version", "status --json"]);
  },
);

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
