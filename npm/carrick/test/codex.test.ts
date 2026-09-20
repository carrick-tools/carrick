// The Codex half of the install: `.codex/hooks.json` written, merged, read and
// removed (carrick#1335).
//
// Every test states its own workspace under a temp directory, because the
// subject is a folder in somebody's repository.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import {
  CODEX_DIR,
  CODEX_HOOKS_FILE,
  codexHooks,
  codexInUse,
  expectedCodexHooks,
  installedCodexHooks,
  isEmptyHooksDocument,
  uninstallCodexHooks,
  writeCodexHooks,
} from "../src/init/codex.ts";
import { checkCodexHooks, findingCount } from "../src/init/doctor.ts";

function workspace(t: { after: (fn: () => void) => void }): string {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-codex-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function hooksFile(root: string): string {
  return path.join(root, CODEX_HOOKS_FILE);
}

function read(root: string): { hooks: Record<string, Array<Record<string, unknown>>> } {
  return JSON.parse(fs.readFileSync(hooksFile(root), "utf8"));
}

test("the two entries Codex needs are the two entries it gets", () => {
  const written = codexHooks();
  // The recording half. `apply_patch` is the tool name Codex puts on the
  // payload, and a matcher is a regex there, so the bare name matches it.
  assert.equal(written["PostToolUse"]![0]!.matcher, "apply_patch");
  assert.equal(written["PostToolUse"]![0]!.hooks[0]!.command, "carrick hook post-edit");
  // One call can carry several files, each a separate check, so this is not the
  // single-file timeout the Claude entry uses.
  assert.equal(written["PostToolUse"]![0]!.hooks[0]!.timeout, 60);
  // The speaking half. Codex refuses `additionalContext` on `Stop`, so the
  // nudge rides the next prompt instead, and this event takes no matcher.
  assert.equal(written["UserPromptSubmit"]![0]!.hooks[0]!.command, "carrick hook user-prompt");
  assert.equal(written["UserPromptSubmit"]![0]!.matcher, undefined);
  assert.equal(written["Stop"], undefined, "a Codex Stop hook could only block");
});

test("a hooks file the user already has keeps everything in it", (t) => {
  const root = workspace(t);
  const theirs = {
    description: "our own hooks",
    hooks: {
      PreToolUse: [{ matcher: "Bash", hooks: [{ type: "command", command: "./scripts/audit.sh" }] }],
      UserPromptSubmit: [{ hooks: [{ type: "command", command: "./scripts/remind.sh" }] }],
    },
  };
  fs.mkdirSync(path.join(root, CODEX_DIR), { recursive: true });
  fs.writeFileSync(hooksFile(root), `${JSON.stringify(theirs, null, 2)}\n`);

  assert.equal(writeCodexHooks(root, "carrick"), "written");
  const merged = read(root);
  assert.equal((merged as unknown as { description: string }).description, "our own hooks");
  assert.equal(merged.hooks["PreToolUse"]!.length, 1);
  // Theirs first, ours after it, on the event they share.
  assert.equal(merged.hooks["UserPromptSubmit"]!.length, 2);
  assert.equal(
    (merged.hooks["UserPromptSubmit"]![0]!["hooks"] as Array<{ command: string }>)[0]!.command,
    "./scripts/remind.sh",
  );

  // And removing ours puts the file back the way it was, byte for byte.
  assert.equal(uninstallCodexHooks(root), "removed");
  assert.deepEqual(JSON.parse(fs.readFileSync(hooksFile(root), "utf8")), theirs);
});

test("writing twice changes nothing the second time", (t) => {
  const root = workspace(t);
  assert.equal(writeCodexHooks(root, "carrick"), "written");
  const first = fs.readFileSync(hooksFile(root), "utf8");
  assert.equal(writeCodexHooks(root, "carrick"), "unchanged");
  assert.equal(fs.readFileSync(hooksFile(root), "utf8"), first);
});

test("a file that was only ever ours goes when the entries do", (t) => {
  const root = workspace(t);
  writeCodexHooks(root, "carrick");
  assert.deepEqual(
    installedCodexHooks(root).map((entry) => entry.command),
    ["carrick hook post-edit", "carrick hook user-prompt"],
  );

  assert.equal(uninstallCodexHooks(root), "file removed");
  assert.equal(fs.existsSync(hooksFile(root)), false);
  // `.codex/` was created by the write and holds nothing else, so it goes too.
  assert.equal(fs.existsSync(path.join(root, CODEX_DIR)), false);
  // Nothing left to remove, and the command stays silent about it.
  assert.equal(uninstallCodexHooks(root), null);
});

test("a .codex holding anything else of Codex's survives the removal", (t) => {
  const root = workspace(t);
  writeCodexHooks(root, "carrick");
  fs.writeFileSync(path.join(root, CODEX_DIR, "config.toml"), "model = \"gpt-5\"\n");

  assert.equal(uninstallCodexHooks(root), "file removed");
  assert.equal(fs.existsSync(hooksFile(root)), false);
  assert.equal(fs.existsSync(path.join(root, CODEX_DIR, "config.toml")), true);
});

test("a document is ours to delete only when nothing but our entries was in it", () => {
  assert.equal(isEmptyHooksDocument('{"hooks":{}}'), true);
  assert.equal(isEmptyHooksDocument("{}"), true);
  assert.equal(isEmptyHooksDocument(""), true);
  // A description, or any key we never write, is somebody's work.
  assert.equal(isEmptyHooksDocument('{"description":"mine","hooks":{}}'), false);
  assert.equal(isEmptyHooksDocument('{"hooks":{"Stop":[{"hooks":[]}]}}'), false);
  assert.equal(isEmptyHooksDocument("not json"), false);
});

test("an install that had to name a path is recognised as ours", (t) => {
  const root = workspace(t);
  const own = '"/opt/my tools/carrick/bin/carrick.mjs"';
  writeCodexHooks(root, own);
  assert.equal(installedCodexHooks(root).length, 2);
  assert.equal(uninstallCodexHooks(root), "file removed");
});

test("a workspace with no .codex is a workspace doctor says nothing about", (t) => {
  const root = workspace(t);
  assert.equal(codexInUse(root), false);
  assert.deepEqual(checkCodexHooks(root), [], "no Codex here, so no line about Codex");

  // A `.codex` and no hook file of ours is the state that is actually broken.
  fs.mkdirSync(path.join(root, CODEX_DIR));
  assert.equal(codexInUse(root), true);
  const missing = checkCodexHooks(root);
  assert.equal(findingCount(missing), 1);
  assert.match(missing[0]!.text, /carrick init/);
});

test("doctor reads the Codex entries the way it reads the Claude ones", (t) => {
  const root = workspace(t);
  writeCodexHooks(root, "carrick");
  const healthy = checkCodexHooks(root);
  assert.equal(findingCount(healthy), 0, "a fresh install is not a finding");
  assert.equal(healthy[0]!.level, "done");

  // An entry this version no longer writes: present, ours, and not the one the
  // current package installs.
  const stale = read(root);
  (stale.hooks["PostToolUse"]![0]!["hooks"] as Array<{ timeout: number }>)[0]!.timeout = 15;
  fs.writeFileSync(hooksFile(root), JSON.stringify(stale, null, 2));
  const drifted = checkCodexHooks(root);
  assert.equal(findingCount(drifted), 1);
  assert.match(drifted[0]!.text, /PostToolUse/);
  assert.equal(
    expectedCodexHooks("carrick").some((entry) => entry.timeout === 15),
    false,
    "the expectation is this version's, not the file's",
  );

  // A file nobody can parse is a refusal, not a silent pass.
  fs.writeFileSync(hooksFile(root), "{ not json");
  const broken = checkCodexHooks(root);
  assert.equal(broken[0]!.level, "refuse");
});
