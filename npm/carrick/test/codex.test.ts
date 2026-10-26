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
import { fileURLToPath } from "node:url";

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
import { outdatedInstall } from "../src/init/outdated.ts";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));

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

test("the entries Codex needs are the entries it gets", () => {
  const written = codexHooks();
  // The orienting half (carrick#2848): the map before the agent's first move,
  // no matcher, the same 30 s the Claude entry has.
  assert.equal(written["SessionStart"]![0]!.hooks[0]!.command, "carrick hook session-start");
  assert.equal(written["SessionStart"]![0]!.hooks[0]!.timeout, 30);
  assert.equal(written["SessionStart"]![0]!.matcher, undefined);
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
    ["carrick hook session-start", "carrick hook post-edit", "carrick hook user-prompt"],
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
  assert.equal(installedCodexHooks(root).length, 3);
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

/** The file an earlier version wrote: the two entries, no SessionStart. */
function writeTwoEntryFile(root: string): void {
  writeCodexHooks(root, "carrick");
  const older = read(root);
  delete older.hooks["SessionStart"];
  fs.writeFileSync(hooksFile(root), `${JSON.stringify(older, null, 2)}\n`);
}

test("an older two-entry file is stale for doctor and outdated, and a re-run adds the entry", (t) => {
  const root = workspace(t);
  fs.mkdirSync(path.join(root, ".carrick"), { recursive: true });
  writeTwoEntryFile(root);

  const drifted = checkCodexHooks(root);
  assert.equal(findingCount(drifted), 1);
  assert.match(drifted[0]!.text, /SessionStart `carrick hook session-start`/);
  assert.deepEqual(outdatedInstall(root).hooks, [CODEX_HOOKS_FILE]);

  assert.equal(writeCodexHooks(root, "carrick"), "written");
  assert.equal(findingCount(checkCodexHooks(root)), 0);
  assert.deepEqual(outdatedInstall(root).hooks, []);
  assert.equal(read(root).hooks["SessionStart"]!.length, 1, "added once, not twice");
});

test("a user's own SessionStart entry stays beside ours and survives the removal", (t) => {
  const root = workspace(t);
  const theirs = {
    hooks: { SessionStart: [{ hooks: [{ type: "command", command: "./scripts/greet.sh" }] }] },
  };
  fs.mkdirSync(path.join(root, CODEX_DIR), { recursive: true });
  fs.writeFileSync(hooksFile(root), `${JSON.stringify(theirs, null, 2)}\n`);

  writeCodexHooks(root, "carrick");
  const merged = read(root).hooks["SessionStart"]!;
  assert.equal(merged.length, 2);
  assert.equal((merged[0]!["hooks"] as Array<{ command: string }>)[0]!.command, "./scripts/greet.sh");

  assert.equal(uninstallCodexHooks(root), "removed");
  assert.deepEqual(JSON.parse(fs.readFileSync(hooksFile(root), "utf8")), theirs);
});

test("the init trust line is true on every Codex surface", () => {
  const source = fs.readFileSync(path.join(packageRoot, "src", "init", "run.ts"), "utf8");
  assert.match(source, /Only the terminal app asks; until you do, Codex skips them without saying so/);
  assert.doesNotMatch(source, /Codex asks you to trust the Carrick hooks the next time it starts/);
});
