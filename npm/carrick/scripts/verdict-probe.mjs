#!/usr/bin/env node
// Does this sidecar produce a type verdict?
//
// The check phase spawns a vendored `pnpm` and a vendored `tsc`. Where those
// live depends on how the sidecar got onto the machine: a checkout has them in
// `src/sidecar/node_modules/.bin`, an npm install has them hoisted above the
// package, because npm strips nested `node_modules` from a published tarball.
// When the lookup misses, nothing crashes — every pair degrades to
// `unverifiable` and the scan reports no mismatch, which reads exactly like the
// types agreeing (carrick#833).
//
// So this drives a given sidecar over its real stdio protocol with two
// hand-authored, dependency-free stubs (the install stays local, no network)
// and asserts the answer is a verdict rather than a degradation. Run it against
// the checkout and against an installed tarball; both must answer the same.
//
//   node scripts/verdict-probe.mjs --sidecar ../../src/sidecar
//   node scripts/verdict-probe.mjs --sidecar /tmp/probe/node_modules/carrick/sidecar
//
// Node builtins only: it runs inside a bare install directory that has this
// package and nothing else.

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

function usage(message) {
  process.stderr.write(`${message}\n\nusage: verdict-probe.mjs --sidecar <dir>\n`);
  process.exit(2);
}

const args = process.argv.slice(2);
const flag = args.indexOf("--sidecar");
if (flag === -1 || !args[flag + 1]) usage("--sidecar is required");
const sidecarDir = path.resolve(args[flag + 1]);
const entry = path.join(sidecarDir, "dist", "src", "index.js");
if (!fs.existsSync(entry)) usage(`no sidecar entry at ${entry}`);

// The stub shape check-v2.test.ts uses: a package whose `types` points at one
// declaration file. No dependencies, so the check's pnpm install is local-only.
function writeStub(root, serviceName, surface) {
  const dir = path.join(root, serviceName);
  fs.mkdirSync(path.join(dir, "types"), { recursive: true });
  fs.writeFileSync(
    path.join(dir, "package.json"),
    `${JSON.stringify(
      {
        name: `@carrick/${serviceName}`,
        version: "0.0.0-carrick",
        private: true,
        types: "./types/surface.d.ts",
      },
      null,
      2,
    )}\n`,
  );
  fs.writeFileSync(path.join(dir, "types", "surface.d.ts"), surface);
  return dir;
}

const root = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-verdict-probe-"));
const producer = writeStub(
  root,
  "orders",
  ["export type Agreed = { a: string; };", "export type Sent = { a: string; };"].join("\n") + "\n",
);
const consumer = writeStub(
  root,
  "web",
  ["export type Agreed = { a: string; };", "export type Expected = { a: string; b: number; };"].join(
    "\n",
  ) + "\n",
);

const request = {
  request_id: "verdict-probe",
  action: "check_v2",
  stubs: [
    { service_name: "orders", stub_dir: producer },
    { service_name: "web", stub_dir: consumer },
  ],
  pairs: [
    {
      pair_key: "agree",
      protocol: "http",
      type_kind: "response",
      producer: { service_name: "orders", alias: "Agreed" },
      consumer: { service_name: "web", alias: "Agreed" },
    },
    {
      pair_key: "differ",
      protocol: "http",
      type_kind: "response",
      producer: { service_name: "orders", alias: "Sent" },
      consumer: { service_name: "web", alias: "Expected" },
    },
  ],
  keep_workspace: false,
};

/** One terminal frame, skipping the install/check keepalives. */
function ask(payload) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [entry], { stdio: ["pipe", "pipe", "pipe"] });
    let buffer = "";
    let stderr = "";
    let settled = false;
    child.stderr.on("data", (d) => (stderr += d.toString()));
    child.stdout.on("data", (d) => {
      buffer += d.toString();
      const lines = buffer.split("\n");
      buffer = lines.pop() ?? "";
      for (const line of lines) {
        if (!line.trim()) continue;
        let frame;
        try {
          frame = JSON.parse(line);
        } catch {
          continue;
        }
        if (frame.request_id !== payload.request_id) continue;
        if (frame.status === "progress") continue;
        settled = true;
        child.kill();
        resolve(frame);
      }
    });
    child.on("error", reject);
    child.on("close", (code) => {
      if (!settled) reject(new Error(`sidecar exited ${code} with no answer\n${stderr}`));
    });
    child.stdin.write(`${JSON.stringify(payload)}\n`);
  });
}

const failures = [];
function expect(label, actual, wanted) {
  if (actual !== wanted) failures.push(`${label}: expected ${wanted}, got ${actual}`);
}

try {
  const frame = await ask(request);
  if (frame.status !== "success") {
    failures.push(
      `check_v2 answered ${frame.status}: ${(frame.errors ?? []).join("; ") || "(no error text)"}`,
    );
  }
  const result = frame.result ?? {};
  const verdicts = new Map((result.verdicts ?? []).map((v) => [v.pair_key, v]));

  // The degradation this probe exists for: a missing vendored pnpm answers
  // `unavailable` and every pair comes back unverifiable.
  expect("isolation", result.isolation, "pnpm");
  expect("install_ok", result.install_ok, true);
  expect("agreeing pair bucket", verdicts.get("agree")?.bucket, "compatible");
  expect("differing pair bucket", verdicts.get("differ")?.bucket, "incompatible");
  // A verdict the judge could not resolve is not a fact, whatever its bucket.
  expect("agreeing pair resolved", verdicts.get("agree")?.resolved, true);

  process.stdout.write(
    `${JSON.stringify({
      sidecar: sidecarDir,
      isolation: result.isolation ?? null,
      install_ok: result.install_ok ?? null,
      ts_version: result.ts_version ?? null,
      buckets: Object.fromEntries([...verdicts].map(([k, v]) => [k, v.bucket])),
      install_error: result.install_error ?? null,
      errors: result.errors ?? [],
    })}\n`,
  );
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}

if (failures.length > 0) {
  process.stderr.write(`verdict probe FAILED for ${sidecarDir}:\n- ${failures.join("\n- ")}\n`);
  process.exit(1);
}
process.stdout.write(`verdict probe OK for ${sidecarDir}\n`);
