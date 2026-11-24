// The reuse block speaks only for a function this edit added (carrick#5121):
// the binary lists every function the tree declares and the index lacks, and a
// checkout ahead of the index makes that every function in the file.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import type { CheckResult } from "../src/contract.ts";
import { addedFromPayload } from "../src/hook/added.ts";
import { similarLines } from "../src/hook/similar.ts";

const SIMILAR = { name: "other", repo: "app", file: "b.ts", line: 3, end_line: 9, score: 0.9, exported: true, called_from: 1 };

/** The binary's list for a file whose three functions the index lacks, plus whatever else is named. */
function answerFor(root: string, functions: Array<[string, number]>): CheckResult {
  return {
    repo: root,
    file: "a.ts",
    recheck: { new_functions: functions.map(([name, line]) => ({ name, line, similar: [SIMILAR] })) },
  } as unknown as CheckResult;
}

function blocks(root: string, functions: Array<[string, number]>, payload: Parameters<typeof addedFromPayload>[0], changes: Parameters<typeof addedFromPayload>[2]) {
  return addedFromPayload(payload, [path.join(root, "a.ts")], changes).then((added) =>
    similarLines(answerFor(root, functions), "a.ts", root, () => true, () => {}, added.get(path.join(root, "a.ts"))).lines,
  );
}

function repo(t: test.TestContext, text: string): string {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-added-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const git = (...args: string[]) => execFileSync("git", args, { cwd: root, stdio: "ignore" });
  git("init", "-q");
  git("config", "user.email", "t@example.com");
  git("config", "user.name", "t");
  fs.writeFileSync(path.join(root, "a.ts"), text);
  git("add", ".");
  git("commit", "-q", "-m", "base");
  return root;
}

const BASE = ["export function alpha() {", "  return 1;", "}", "export function beta() {", "  return 2;", "}", "export function gamma() {", "  return 3;", "}", ""].join("\n");
const THREE: Array<[string, number]> = [["alpha", 1], ["beta", 4], ["gamma", 7]];

test("an edit that adds one constant to a file with three functions the index lacks prints no block", async (t) => {
  const root = repo(t, BASE);
  fs.writeFileSync(path.join(root, "a.ts"), `export const LIMIT = 5;\n${BASE}`);
  const out = await blocks(root, [["alpha", 2], ["beta", 5], ["gamma", 8]], {}, new Map([[path.join(root, "a.ts"), { lines: [1], removedText: [] }]]));
  assert.deepEqual(out, []);
});

test("an edit that changes a line inside an existing function prints no block", async (t) => {
  const root = repo(t, BASE);
  fs.writeFileSync(path.join(root, "a.ts"), BASE.replace("return 2;", "return 20;"));
  const out = await blocks(root, THREE, {}, new Map([[path.join(root, "a.ts"), { lines: [5], removedText: ["  return 2;"] }]]));
  assert.deepEqual(out, []);
});

test("an edit that rewrites the opening line of an existing function prints no block", async (t) => {
  const root = repo(t, BASE);
  fs.writeFileSync(path.join(root, "a.ts"), BASE.replace("beta()", "beta(x: number)"));
  const out = await blocks(root, THREE, {}, new Map([[path.join(root, "a.ts"), { lines: [4], removedText: ["export function beta() {"] }]]));
  assert.deepEqual(out, []);
});

test("an edit that adds a new function prints exactly one block, for that function", async (t) => {
  const root = repo(t, BASE);
  fs.writeFileSync(path.join(root, "a.ts"), `${BASE}export function delta() {\n  return 4;\n}\n`);
  const out = await blocks(root, [...THREE, ["delta", 10]], {}, new Map([[path.join(root, "a.ts"), { lines: [10, 11, 12], removedText: [] }]]));
  assert.equal(out.length, 1);
  assert.match(out[0]!, /^Carrick: `delta` may already exist/);
});

test("a Write that creates a file prints a block for each of its functions", async (t) => {
  const root = repo(t, BASE);
  const out = await blocks(root, THREE, { tool_name: "Write", tool_response: { type: "create" } }, undefined);
  assert.equal(out.length, 3);
});

test("a Write over an existing file is compared with the file it replaced", async (t) => {
  const root = repo(t, BASE);
  fs.writeFileSync(path.join(root, "a.ts"), `${BASE}export function delta() {\n  return 4;\n}\n`);
  const payload = { tool_name: "Write", tool_response: { type: "update", originalFile: BASE } };
  const out = await blocks(root, [...THREE, ["delta", 10]], payload, undefined);
  assert.equal(out.length, 1);
  assert.match(out[0]!, /`delta`/);
});

test("with no patch, a function present at HEAD prints no block and one absent from HEAD does", async (t) => {
  const root = repo(t, BASE);
  fs.writeFileSync(path.join(root, "a.ts"), `${BASE}export function delta() {\n  return 4;\n}\n`);
  const out = await blocks(root, [...THREE, ["delta", 10]], { tool_name: "Bash" }, undefined);
  assert.equal(out.length, 1);
  assert.match(out[0]!, /`delta`/);
});

test("with no patch and no git, nothing is said", async (t) => {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-nogit-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.writeFileSync(path.join(root, "a.ts"), BASE);
  assert.deepEqual(await blocks(root, THREE, { tool_name: "Bash" }, undefined), []);
});

test("with no patch, functions all present at HEAD print no block", async (t) => {
  const root = repo(t, BASE);
  assert.deepEqual(await blocks(root, THREE, { tool_name: "Bash" }, undefined), []);
});
