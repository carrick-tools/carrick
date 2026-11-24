// Every per-engine file names its checkout (#5038, W1): starting an init that names a linked worktree
// leaves the main checkout's shadow folder and placed file as they were. A fake compiler engine; invented names.

import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import type { ServiceInit } from "../src/held/daemon.ts";
import { placedPath, shadowDir } from "../src/held/paths.ts";
import { createRuntime } from "../src/held/runtime.ts";

const FAKE_ENGINE = `
import fs from "node:fs";
import path from "node:path";
const lines = fs.readFileSync(0, "utf8").split("\\n").filter(Boolean).map((l) => JSON.parse(l));
for (const r of lines) {
  if (r.action === "init") { console.log(JSON.stringify({ request_id: r.request_id, status: "ready" })); continue; }
  if (r.action !== "write_shadow") continue;
  fs.mkdirSync(path.join(r.out_dir, "w"), { recursive: true });
  fs.writeFileSync(path.join(r.out_dir, "w", "tsconfig.json"), "{}");
  fs.writeFileSync(path.join(r.out_dir, "w", "own.ts"), "from " + r.out_dir);
  const fit = { strictNullChecks: true, strictFunctionTypes: true, exactOptionalPropertyTypes: false };
  console.log(JSON.stringify({
    request_id: r.request_id, status: "success", tsconfig: path.join(r.out_dir, "w", "tsconfig.json"),
    own_files: ["own.ts"], native_tsc: "/bin/true",
    wire: ["type __carrick_Wire<S, E> = S;"], tolerances: { unwidened_alias_suffix: "", declarations: [] },
    options: { service: fit, judge: fit },
  }));
}
`;

/** One checksum over every file under `dir` (path and bytes), or null when it is not there. */
function sum(dir: string): string | null {
  if (!fs.existsSync(dir)) return null;
  const hash = createHash("sha256");
  const walk = (d: string): void => {
    for (const name of fs.readdirSync(d).sort()) {
      const file = path.join(d, name);
      if (fs.statSync(file).isDirectory()) walk(file);
      else hash.update(file).update(fs.readFileSync(file));
    }
  };
  walk(dir);
  return hash.digest("hex");
}

function world(t: { after: (fn: () => void) => void }) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "held-checkout-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const root = path.join(dir, "ws");
  const top = path.join(dir, "linked");
  const home = path.join(dir, "home");
  for (const base of [root, top]) fs.mkdirSync(path.join(base, "src"), { recursive: true });
  const entry = path.join(dir, "engine.mjs");
  fs.writeFileSync(entry, FAKE_ENGINE);
  const main: ServiceInit = { repo: "shop", service: "orders", repo_path: root, sidecar_root: root, tsconfig: null, scan_root: root };
  const other: ServiceInit = { ...main, repo_path: top, sidecar_root: top, scan_root: top, checkout: top };
  // No rows: the start writes the shadow and the placed file, then answers with an idle service.
  const runtime = createRuntime({ root, home, entry, env: { ...process.env }, rows: async () => [], stamp: () => "s", commits: async () => null, repoPaths: async () => ({}) });
  return { root, top, home, main, other, runtime };
}

test("W1: starting an init that names a checkout leaves the main shadow folder and placed file untouched", async (t) => {
  const w = world(t);
  const first = await w.runtime.start(w.main);
  assert.ok("engine" in first, JSON.stringify(first));
  const mainShadow = shadowDir(w.root, "orders", w.home);
  const mainPlaced = placedPath(w.root, "orders", w.home);
  const before = { shadow: sum(mainShadow), placed: fs.readFileSync(mainPlaced, "utf8") };
  assert.notEqual(before.shadow, null);
  const second = await w.runtime.start(w.other);
  assert.ok("engine" in second, JSON.stringify(second));
  assert.equal(sum(mainShadow), before.shadow, "main's shadow is the same bytes");
  assert.equal(fs.readFileSync(mainPlaced, "utf8"), before.placed, "main's placed file is the same bytes");
  assert.notEqual(sum(shadowDir(w.top, "orders", w.home)), null, "the checkout's own shadow exists");
  assert.ok(fs.existsSync(placedPath(w.top, "orders", w.home)), "the checkout's own placed file exists");
});

test("W1: a main-only start writes the shadow and placed file at the exact main paths", async (t) => {
  const w = world(t);
  await w.runtime.start(w.main);
  const key = createHash("sha256").update(fs.realpathSync(w.root)).digest("hex").slice(0, 12);
  const seg = `orders-${createHash("sha256").update("orders").digest("hex").slice(0, 6)}`;
  const base = path.join(w.home, ".carrick", "held", key);
  assert.equal(shadowDir(w.root, "orders", w.home), path.join(base, seg));
  assert.ok(fs.existsSync(path.join(base, seg, "w", "tsconfig.json")));
  assert.ok(fs.existsSync(path.join(base, `${seg}.placed.json`)));
});
