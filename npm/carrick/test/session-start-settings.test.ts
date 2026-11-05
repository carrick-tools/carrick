// Session start brings Carrick's own installed files up to date
// (`src/init/self-update.ts`), so the "run `carrick init`" line is not what an
// upgrade costs a person.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import path from "node:path";

import { fakeEnv, fixturePath, makeWorkspace, runHook } from "./helpers.ts";
import { currentVersion } from "../src/update.ts";
import { CODEX_HOOKS_FILE, codexHooks } from "../src/init/codex.ts";
import { carrickHooks, mergeCarrickHooks } from "../src/init/settings.ts";
import { updateHooks } from "../src/init/self-update.ts";
import { skillFile, SKILL_ROOTS, stamped, writeTaskSkills } from "../src/init/task-skills.ts";

const LOCAL = path.join(".claude", "settings.local.json");
const SHARED = path.join(".claude", "settings.json");
const running = currentVersion()!;

type Workspace = ReturnType<typeof makeWorkspace>;

function put(root: string, relative: string, body: string): string {
  const target = path.join(root, relative);
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.writeFileSync(target, body);
  return target;
}

/** Our entries as an older version wrote them: post-edit with a former timeout, beside a user's own hook. */
function olderSettings(prefix = "carrick"): string {
  const document = JSON.parse(mergeCarrickHooks(null, prefix).body);
  for (const groups of Object.values(document.hooks) as { hooks: { command: string; timeout?: number }[] }[][]) {
    for (const group of groups) {
      for (const entry of group.hooks) if (/hook post-edit/.test(entry.command)) entry.timeout = 10;
    }
  }
  document.hooks.PostToolUse.push({ matcher: "Bash", hooks: [{ type: "command", command: "echo mine" }] });
  document.permissions = { allow: ["Read"] };
  return `${JSON.stringify(document, null, 2)}\n`;
}

function session(workspace: Workspace, extra: Record<string, string> = {}) {
  return runHook("session-start.ts", {
    cwd: workspace.root,
    payload: { hook_event_name: "SessionStart" },
    env: fakeEnv({
      CLAUDE_PROJECT_DIR: workspace.root,
      CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json"),
      ...extra,
    }),
  });
}

function recordVersion(root: string, version: string): void {
  put(root, path.join(".carrick", "cli-version"), `${version}\n`);
}

const HOOK_LINE = new RegExp(`Carrick brought its hook entries in \\S+ up to date for ${running.replaceAll(".", "\\.")}\\.`);

test("a session start rewrites our stale entries, keeps the user's hook and keys, prints one line", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const file = put(workspace.root, LOCAL, olderSettings());

  const run = await session(workspace);
  assert.equal(run.code, 0);
  assert.match(run.stdout, HOOK_LINE);
  assert.equal(run.stdout.match(/brought its hook entries/g)?.length, 1);

  const after = JSON.parse(fs.readFileSync(file, "utf8"));
  const expected = carrickHooks("carrick");
  const ours = (event: string) =>
    (after.hooks[event] as { hooks: { command: string; timeout?: number }[] }[]).filter((group) =>
      group.hooks.some((entry) => /\bhook\b/.test(entry.command) && !entry.command.startsWith("echo")),
    );
  for (const [event, groups] of Object.entries(expected)) {
    assert.deepEqual(ours(event), groups, `${event} entries are the ones this version writes`);
  }
  assert.deepEqual(after.permissions, { allow: ["Read"] });
  assert.ok(JSON.stringify(after.hooks.PostToolUse).includes("echo mine"), "the user's hook stays");
});

test("a stale-looking file the merge leaves as it is is neither stamped nor reported", (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const body = olderSettings();
  const file = put(workspace.root, LOCAL, body);

  const reported = updateHooks(workspace.root, workspace.root, running, (existing) => ({
    body: existing ?? "",
    changed: false,
  }));

  assert.deepEqual(reported, []);
  assert.equal(fs.readFileSync(file, "utf8"), body);
  assert.equal(fs.existsSync(path.join(workspace.root, ".carrick", "hook-entries-updated.json")), false);
});

test("a second session start writes nothing and prints nothing about it", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const file = put(workspace.root, LOCAL, olderSettings());
  await session(workspace);
  const bytes = fs.readFileSync(file, "utf8");
  const mtime = fs.statSync(file).mtimeMs;

  const second = await session(workspace);
  assert.doesNotMatch(second.stdout, /brought its/);
  assert.equal(fs.readFileSync(file, "utf8"), bytes);
  assert.equal(fs.statSync(file).mtimeMs, mtime);
});

test("a Codex hook file with old entries is byte-identical after", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  put(workspace.root, LOCAL, olderSettings());
  const old = JSON.parse(JSON.stringify({ hooks: codexHooks("carrick") }));
  for (const groups of Object.values(old.hooks) as { hooks: { timeout?: number }[] }[][]) groups[0]!.hooks[0]!.timeout = 99;
  const codexBody = `${JSON.stringify(old, null, 2)}\n`;
  const codexFile = put(workspace.root, CODEX_HOOKS_FILE, codexBody);

  await session(workspace);
  assert.equal(fs.readFileSync(codexFile, "utf8"), codexBody);
});

test("an absolute-path prefix is kept", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const prefix = "/opt/tools/carrick.mjs";
  const file = put(workspace.root, LOCAL, olderSettings(prefix));

  await session(workspace);
  const body = fs.readFileSync(file, "utf8");
  assert.match(body, /\/opt\/tools\/carrick\.mjs hook session-start/);
  assert.doesNotMatch(body, /"command": "carrick hook/);
});

test("a settings file with no entry of ours is untouched", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const body = '{"hooks":{"PostToolUse":[{"hooks":[{"type":"command","command":"echo mine"}]}]}}';
  const file = put(workspace.root, SHARED, body);
  const mtime = fs.statSync(file).mtimeMs;

  const run = await session(workspace);
  assert.doesNotMatch(run.stdout, /brought its/);
  assert.equal(fs.readFileSync(file, "utf8"), body);
  assert.equal(fs.statSync(file).mtimeMs, mtime);
});

test("invalid JSON is untouched and nothing is printed about it", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const body = '{"hooks": {,}';
  const file = put(workspace.root, LOCAL, body);

  const run = await session(workspace);
  assert.equal(run.code, 0);
  assert.doesNotMatch(run.stdout, /brought its/);
  assert.equal(fs.readFileSync(file, "utf8"), body);
});

test("a running version older than the recorded one rewrites nothing", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const file = put(workspace.root, LOCAL, olderSettings());
  writeTaskSkills(workspace.root, { slug: null });
  const skill = path.join(workspace.root, skillFile(SKILL_ROOTS[0]!, "carrick-impact"));
  fs.writeFileSync(skill, stamped("an older body"));
  recordVersion(workspace.root, "999.0.0");
  const hooks = fs.readFileSync(file, "utf8");

  const run = await session(workspace);
  assert.doesNotMatch(run.stdout, /brought its/);
  assert.equal(fs.readFileSync(file, "utf8"), hooks);
  assert.equal(fs.readFileSync(skill, "utf8"), stamped("an older body"));
});

test("a Codex payload (no CLAUDE_PROJECT_DIR) rewrites nothing", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const file = put(workspace.root, LOCAL, olderSettings());
  const before = fs.readFileSync(file, "utf8");

  const run = await runHook("session-start.ts", {
    cwd: workspace.root,
    payload: { hook_event_name: "SessionStart", session_id: "s", cwd: workspace.root },
    env: fakeEnv({ CLAUDE_PROJECT_DIR: "", CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json") }),
  });
  assert.doesNotMatch(run.stdout, /brought its/);
  assert.equal(fs.readFileSync(file, "utf8"), before);
});

test("an entry removed on purpose after an update stays removed in the same version", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const file = put(workspace.root, LOCAL, olderSettings());
  await session(workspace);
  const document = JSON.parse(fs.readFileSync(file, "utf8"));
  delete document.hooks.Stop;
  const trimmed = `${JSON.stringify(document, null, 2)}\n`;
  fs.writeFileSync(file, trimmed);

  await session(workspace);
  assert.equal(fs.readFileSync(file, "utf8"), trimmed);
});

test("skills still ours are brought up to date under both roots, with their own scope, and the mismatch line goes", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  writeTaskSkills(workspace.root, { slug: "acme-index" });
  const claude = path.join(workspace.root, skillFile(SKILL_ROOTS[0]!, "carrick-impact"));
  const agents = path.join(workspace.root, skillFile(SKILL_ROOTS[1]!, "carrick-impact"));
  const current = fs.readFileSync(claude, "utf8");
  for (const old of [claude, agents]) fs.writeFileSync(old, stamped('an older body that says project: "acme-index"'));
  recordVersion(workspace.root, "0.0.1");

  const run = await session(workspace);
  assert.match(run.stdout, new RegExp(`Carrick brought its task skills in \\.claude/skills, \\.agents/skills up to date for ${running.replaceAll(".", "\\.")}\\.`));
  assert.doesNotMatch(run.stdout, /carrick init/);
  assert.equal(fs.readFileSync(claude, "utf8"), current);
  assert.equal(fs.readFileSync(agents, "utf8"), current);
  assert.equal(fs.readFileSync(path.join(workspace.root, ".carrick", "cli-version"), "utf8").trim(), running);

  const second = await session(workspace);
  assert.doesNotMatch(second.stdout, /brought its|carrick init/);
});

test("an edited skill is left alone, and a missing one is not created", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  writeTaskSkills(workspace.root, { slug: null });
  const edited = path.join(workspace.root, skillFile(SKILL_ROOTS[0]!, "carrick-impact"));
  const body = `${stamped("an older body")}my own step\n`;
  fs.writeFileSync(edited, body);
  const missing = path.join(workspace.root, skillFile(SKILL_ROOTS[0]!, "carrick-reuse"));
  fs.rmSync(missing);

  await session(workspace);
  assert.equal(fs.readFileSync(edited, "utf8"), body);
  assert.ok(!fs.existsSync(missing));
});

test("old Codex hooks keep the mismatch line honest: the version is not recorded over them", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  put(workspace.root, LOCAL, olderSettings());
  fs.mkdirSync(path.join(workspace.root, ".codex"), { recursive: true });
  const old = JSON.parse(JSON.stringify({ hooks: codexHooks("carrick") }));
  for (const groups of Object.values(old.hooks) as { hooks: { timeout?: number }[] }[][]) groups[0]!.hooks[0]!.timeout = 99;
  put(workspace.root, CODEX_HOOKS_FILE, `${JSON.stringify(old, null, 2)}\n`);
  recordVersion(workspace.root, "0.0.1");

  const run = await session(workspace);
  assert.match(run.stdout, /Run `carrick init`/);
  assert.equal(fs.readFileSync(path.join(workspace.root, ".carrick", "cli-version"), "utf8").trim(), "0.0.1");
});
