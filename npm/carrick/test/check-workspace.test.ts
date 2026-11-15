// The hook's root is the one `.carrick/` folder read (carrick#1534): a parent
// `.carrick/` at commit B and a nested repo's own at commit A. A check passes
// the root as `--workspace`, so the answer names B whatever the file's ancestors hold.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { check, checkEach } from "../src/cli.ts";
import { probeEdits } from "../src/hook/probe.ts";
import { fixturePath, testDir } from "./helpers.ts";

const bin = path.join(testDir, "fake-nested-index.mjs");

function nested(t: { after: (fn: () => void) => void }) {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-ws-")));
  t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  const repo = path.join(base, "inventory-svc");
  fs.mkdirSync(path.join(repo, "src"), { recursive: true });
  fs.writeFileSync(path.join(repo, "src", "client.ts"), "export const a = 1;\n");
  for (const [dir, commit] of [[base, "B"], [repo, "A"]] as const) {
    fs.mkdirSync(path.join(dir, ".carrick"));
    fs.writeFileSync(path.join(dir, ".carrick", "index.json"), JSON.stringify({ commit }));
  }
  const env = { ...process.env, CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json") };
  return { base, env, relative: path.join("inventory-svc", "src", "client.ts") };
}

test("check with a workspace answers from the root's index, not the nested one", async (t) => {
  const w = nested(t);
  const out = await check(w.relative, { cwd: w.base, workspace: w.base, env: w.env, bin });
  assert.equal(out.result?.index_commit, "B");
});

test("check --each with a workspace answers from the root's index", async (t) => {
  const w = nested(t);
  const out = await checkEach([w.relative, w.relative], {
    cwd: w.base, workspace: w.base, env: w.env, bin, limitMs: 5000,
  });
  assert.deepEqual(out.result?.map((r) => r?.index_commit), ["B", "B"]);
});

test("the post-edit probe passes its root as the workspace", async (t) => {
  const w = nested(t);
  const one = await probeEdits([w.relative], w.base, { ...w.env, CARRICK_BIN: bin });
  assert.equal(one.results[0]?.index_commit, "B");
  const two = await probeEdits([w.relative, w.relative], w.base, { ...w.env, CARRICK_BIN: bin });
  assert.deepEqual(two.results.map((r) => r?.index_commit), ["B", "B"]);
});
