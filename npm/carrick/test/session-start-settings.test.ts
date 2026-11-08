// Session start brings Carrick's own installed files up to date
// (`src/init/self-update.ts`), so the "run `carrick init`" line is not what an
// upgrade costs a person.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";

import { fakeEnv, fixturePath, makeWorkspace, runHook } from "./helpers.ts";
import { currentVersion } from "../src/update.ts";
import { CODEX_HOOKS_FILE, codexHooks } from "../src/init/codex.ts";
import { carrickHooks, mergeCarrickHooks } from "../src/init/settings.ts";
import { renderCheckSection } from "../src/init/agents-section.ts";
import { bringUpToDate, updateHooks } from "../src/init/self-update.ts";
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

test("a Codex payload (no CLAUDE_PROJECT_DIR) leaves the Claude Code settings files alone", async (t) => {
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

test("a session started in a repo of the folder rewrites that repo's entries after the folder's own", (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const repo = path.join(workspace.root, "api");
  const own = put(workspace.root, LOCAL, olderSettings());
  const inRepo = put(repo, LOCAL, olderSettings());

  assert.deepEqual(updateHooks(workspace.root, workspace.root, running), [LOCAL]);
  assert.deepEqual(updateHooks(workspace.root, repo, running), [LOCAL]);

  const expected = carrickHooks("carrick");
  for (const file of [own, inRepo]) {
    const after = JSON.parse(fs.readFileSync(file, "utf8"));
    assert.deepEqual(after.hooks.Stop, expected.Stop, `${file} holds this version's entries`);
  }
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

function git(repo: string, ...args: string[]): void {
  execFileSync("git", ["-C", repo, "-c", "user.email=t@example.com", "-c", "user.name=t", ...args], { stdio: "ignore" });
}

function heldLine(files: string[]): RegExp {
  const named = files.map((file) => file.replaceAll(".", "\\.")).join(" and ");
  return new RegExp(
    `Carrick did not update ${named} because git tracks them here\\. Run \`carrick init\`, then commit the change in its own pull request\\.`,
  );
}

test("a tracked stale skill and settings file are left byte for byte and named in one line", (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const settings = put(workspace.root, LOCAL, olderSettings());
  const skillPath = skillFile(SKILL_ROOTS[0]!, "carrick-impact");
  const skill = put(workspace.root, skillPath, stamped("an older body"));
  recordVersion(workspace.root, "0.0.1");
  git(workspace.root, "init", "-q");
  git(workspace.root, "add", LOCAL, skillPath);
  git(workspace.root, "commit", "-q", "-m", "tracked");
  const settingsBefore = fs.readFileSync(settings, "utf8");
  const skillBefore = fs.readFileSync(skill, "utf8");

  const lines = bringUpToDate(workspace.root, workspace.root, running);

  assert.equal(fs.readFileSync(settings, "utf8"), settingsBefore);
  assert.equal(fs.readFileSync(skill, "utf8"), skillBefore);
  assert.equal(lines.length, 1);
  assert.match(lines[0]!, heldLine([LOCAL, skillPath]));
  assert.equal(fs.readFileSync(path.join(workspace.root, ".carrick", "cli-version"), "utf8").trim(), "0.0.1");
});

test("when every outdated file is tracked, the held line replaces the version mismatch line", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  put(workspace.root, LOCAL, olderSettings());
  recordVersion(workspace.root, "0.0.1");
  git(workspace.root, "init", "-q");
  git(workspace.root, "add", LOCAL);
  git(workspace.root, "commit", "-q", "-m", "tracked");

  const run = await session(workspace);

  assert.match(run.stdout, heldLine([LOCAL]));
  assert.doesNotMatch(run.stdout, /set this workspace up/);
});

test("the same files untracked are rewritten, as before", (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const settings = put(workspace.root, LOCAL, olderSettings());
  const skillPath = skillFile(SKILL_ROOTS[0]!, "carrick-impact");
  const skill = put(workspace.root, skillPath, stamped("an older body"));
  git(workspace.root, "init", "-q");

  const lines = bringUpToDate(workspace.root, workspace.root, running);

  assert.notEqual(fs.readFileSync(settings, "utf8"), olderSettings());
  assert.notEqual(fs.readFileSync(skill, "utf8"), stamped("an older body"));
  assert.ok(lines.some((line) => /brought its hook entries/.test(line)));
  assert.ok(lines.every((line) => !/did not update/.test(line)));
});

test("a repo below a folder keeps its tracked settings file byte for byte and names it", (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const repo = path.join(workspace.root, "api");
  const settings = put(repo, LOCAL, olderSettings());
  git(repo, "init", "-q");
  git(repo, "add", LOCAL);
  git(repo, "commit", "-q", "-m", "tracked");
  const before = fs.readFileSync(settings, "utf8");

  const lines = bringUpToDate(workspace.root, repo, running);

  assert.equal(fs.readFileSync(settings, "utf8"), before);
  assert.equal(lines.length, 1);
  assert.match(lines[0]!, heldLine([path.join("api", LOCAL)]));
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

// The Carrick rules section in AGENTS.md follows the CLI (carrick-cloud#2991).
const OLD_SECTION = ["<!-- carrick:check start -->", "## Carrick: an older rule set", "1. Old rule.", "<!-- carrick:check end -->", ""].join("\n");
const SURROUND_BEFORE = "# Shop notes\n\nKeep handlers small.\n\n";
const SURROUND_AFTER = "\n## Other\n\nStay kind.\n";

function snapshot(root: string, repos: unknown): void {
  put(root, path.join(".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos } }));
}

const TWO_REPOS = [
  { project_slug: "shop-system", services: Array.from({ length: 20 }, (_, i) => `a${i}`) },
  { project_slug: "shop-system", services: Array.from({ length: 23 }, (_, i) => `b${i}`) },
];

test("self-update rewrites our AGENTS.md section for the snapshot's project and counts", (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  put(workspace.root, "AGENTS.md", SURROUND_BEFORE + OLD_SECTION + SURROUND_AFTER);
  snapshot(workspace.root, TWO_REPOS);
  const lines = bringUpToDate(workspace.root, workspace.root, running);
  assert.equal(
    fs.readFileSync(path.join(workspace.root, "AGENTS.md"), "utf8"),
    SURROUND_BEFORE + renderCheckSection({ slug: "shop-system" }, { repos: 2, services: 43 }) + SURROUND_AFTER,
  );
  assert.ok(lines.some((line) => /rules section in AGENTS\.md/.test(line)));
});

test("self-update never adds the AGENTS.md section where the markers are absent", (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  put(workspace.root, "AGENTS.md", SURROUND_BEFORE);
  snapshot(workspace.root, TWO_REPOS);
  bringUpToDate(workspace.root, workspace.root, running);
  assert.equal(fs.readFileSync(path.join(workspace.root, "AGENTS.md"), "utf8"), SURROUND_BEFORE);
  const absent = makeWorkspace();
  t.after(() => absent.cleanup());
  snapshot(absent.root, TWO_REPOS);
  bringUpToDate(absent.root, absent.root, running);
  assert.ok(!fs.existsSync(path.join(absent.root, "AGENTS.md")));
});

test("self-update leaves a current AGENTS.md section unwritten", (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const target = put(
    workspace.root,
    "AGENTS.md",
    SURROUND_BEFORE + renderCheckSection({ slug: "shop-system" }, { repos: 2, services: 43 }),
  );
  snapshot(workspace.root, TWO_REPOS);
  const past = new Date(1_000_000);
  fs.utimesSync(target, past, past);
  const lines = bringUpToDate(workspace.root, workspace.root, running);
  assert.equal(fs.statSync(target).mtimeMs, past.getTime());
  assert.ok(!lines.some((line) => /rules section/.test(line)));
});

test("self-update leaves the AGENTS.md section alone, and says why, without a readable snapshot", (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const body = SURROUND_BEFORE + OLD_SECTION;
  put(workspace.root, "AGENTS.md", body);
  const said: string[] = [];
  bringUpToDate(workspace.root, workspace.root, running, (message) => said.push(message));
  assert.equal(fs.readFileSync(path.join(workspace.root, "AGENTS.md"), "utf8"), body);
  assert.ok(said.some((message) => /AGENTS\.md section left/.test(message)));
});

test("self-update adds the block to a CLAUDE.md created after init, refreshes a stale one, and never touches it without the AGENTS.md marker (carrick#3515)", (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const current = renderCheckSection({ slug: "shop-system" }, { repos: 2, services: 43 });
  put(workspace.root, "AGENTS.md", current);
  put(workspace.root, "CLAUDE.md", "# Team\n");
  snapshot(workspace.root, TWO_REPOS);
  const lines = bringUpToDate(workspace.root, workspace.root, running);
  assert.equal(fs.readFileSync(path.join(workspace.root, "CLAUDE.md"), "utf8"), `# Team\n\n${current}`);
  assert.ok(lines.some((line) => /rules section in CLAUDE\.md/.test(line)), lines.join("\n"));
  put(workspace.root, "CLAUDE.md", SURROUND_BEFORE + OLD_SECTION + SURROUND_AFTER);
  bringUpToDate(workspace.root, workspace.root, running);
  assert.equal(fs.readFileSync(path.join(workspace.root, "CLAUDE.md"), "utf8"), SURROUND_BEFORE + current + SURROUND_AFTER);

  const bare = makeWorkspace();
  t.after(() => bare.cleanup());
  put(bare.root, "AGENTS.md", "# No marker\n");
  put(bare.root, "CLAUDE.md", "# Team\n");
  snapshot(bare.root, TWO_REPOS);
  bringUpToDate(bare.root, bare.root, running);
  assert.equal(fs.readFileSync(path.join(bare.root, "CLAUDE.md"), "utf8"), "# Team\n");
});

// A workspace used only from Codex gets the same rules refresh (carrick#3435):
// Codex reads AGENTS.md directly, and only `.codex/hooks.json` has a trust gate.
test("a Codex session start rewrites the AGENTS.md section and skills, never .codex/hooks.json", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  put(workspace.root, "AGENTS.md", SURROUND_BEFORE + OLD_SECTION + SURROUND_AFTER);
  snapshot(workspace.root, TWO_REPOS);
  writeTaskSkills(workspace.root, { slug: "shop-system" });
  const agentsSkill = path.join(workspace.root, skillFile(SKILL_ROOTS[1]!, "carrick-impact"));
  const currentSkill = fs.readFileSync(agentsSkill, "utf8");
  fs.writeFileSync(agentsSkill, stamped('an older body that says project: "shop-system"'));
  const old = JSON.parse(JSON.stringify({ hooks: codexHooks("carrick") }));
  for (const groups of Object.values(old.hooks) as { hooks: { timeout?: number }[] }[][]) groups[0]!.hooks[0]!.timeout = 99;
  const codexBody = `${JSON.stringify(old, null, 2)}\n`;
  const codexFile = put(workspace.root, CODEX_HOOKS_FILE, codexBody);
  recordVersion(workspace.root, "0.0.1");

  const run = await runHook("session-start.ts", {
    cwd: workspace.root,
    payload: { hook_event_name: "SessionStart", session_id: "s", cwd: workspace.root },
    env: fakeEnv({ CLAUDE_PROJECT_DIR: "", CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json") }),
  });
  assert.equal(run.code, 0);
  const context = (JSON.parse(run.stdout) as { hookSpecificOutput: { additionalContext: string } }).hookSpecificOutput
    .additionalContext;
  assert.match(context, /rules section in AGENTS\.md up to date/);
  assert.match(context, /task skills in \.agents\/skills up to date/);
  assert.equal(
    fs.readFileSync(path.join(workspace.root, "AGENTS.md"), "utf8"),
    SURROUND_BEFORE + renderCheckSection({ slug: "shop-system" }, { repos: 2, services: 43 }) + SURROUND_AFTER,
  );
  assert.equal(fs.readFileSync(agentsSkill, "utf8"), currentSkill);
  assert.equal(fs.readFileSync(codexFile, "utf8"), codexBody);
  // The old Codex hooks are still ours to replace, so the mismatch line stays.
  assert.match(context, /Run `carrick init`/);
  assert.equal(fs.readFileSync(path.join(workspace.root, ".carrick", "cli-version"), "utf8").trim(), "0.0.1");
});
