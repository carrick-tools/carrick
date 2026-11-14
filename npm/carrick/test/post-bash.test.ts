// The Bash hook (carrick hook post-bash): a shell command that reads a
// TypeScript file gets the Read hook's answer.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { bashTargets, movesTreeInBulk, searchedNames } from "../src/hook/bash-targets.ts";
import { bringUpToDate, updateHooks } from "../src/init/self-update.ts";
import { carrickHooks } from "../src/init/settings.ts";
import { editPayload, fakeEnv, fixturePath, makeWorkspace, runHook } from "./helpers.ts";

const USES_LINE =
  "- CatalogClient.readWidget (line 12) has 2 caller(s) outside this file: src/inventory.ts:9, src/inventory.ts:17";

function git(dir: string, ...args: string[]): void {
  execFileSync("git", args, {
    cwd: dir,
    stdio: "ignore",
    env: { ...process.env, GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t" },
  });
}

/** An indexed repo with a linked worktree, a TS file and a markdown file in each. */
function repoWithWorktree(t: { after: (fn: () => void) => void }) {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-pb-")));
  t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  const home = path.join(base, "home");
  fs.mkdirSync(home);
  const repo = path.join(base, "svc");
  fs.mkdirSync(path.join(repo, "src", "capture"), { recursive: true });
  fs.writeFileSync(path.join(repo, "src", "capture", "backfill.ts"), "export const a = 1;\n");
  fs.writeFileSync(path.join(repo, "NOTES.md"), "notes\n");
  fs.writeFileSync(path.join(repo, ".gitignore"), ".carrick/\n.claude/worktrees/\n");
  git(repo, "init", "-q", "-b", "main");
  git(repo, "add", ".");
  git(repo, "commit", "-q", "-m", "init");
  fs.mkdirSync(path.join(repo, ".carrick"));
  git(repo, "worktree", "add", "-q", ".claude/worktrees/wt", "-b", "wt");
  const worktree = path.join(repo, ".claude", "worktrees", "wt");
  return { base, home, repo, worktree, file: path.join(worktree, "src", "capture", "backfill.ts") };
}

function bashPayload(cwd: string, command: string, session: string) {
  return {
    hook_event_name: "PostToolUse",
    tool_name: "Bash",
    session_id: session,
    cwd,
    tool_input: { command },
    tool_response: { stdout: "..." },
  };
}

function contextOf(stdout: string): string {
  if (!stdout) return "";
  return (JSON.parse(stdout) as { hookSpecificOutput: { additionalContext: string } })
    .hookSpecificOutput.additionalContext;
}

function runs(argvLog: string): number {
  return fs.existsSync(argvLog) ? fs.readFileSync(argvLog, "utf8").split("\n").filter((l) => l.trim()).length : 0;
}

test("a sed read of a TS file in a worktree prints the callers line", async (t) => {
  const w = repoWithWorktree(t);
  const argvLog = path.join(w.base, "argv.log");
  const run = await runHook("post-bash.ts", {
    payload: bashPayload(w.worktree, "cd src && sed -n 1,140p capture/backfill.ts", "sess-b1"),
    env: fakeEnv({ HOME: w.home, CARRICK_FAKE_ARGV_LOG: argvLog, CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json") }),
  });
  assert.equal(run.code, 0);
  assert.match(contextOf(run.stdout), /^Carrick: other code depends on/);
  assert.ok(contextOf(run.stdout).includes(USES_LINE), run.stdout);
  assert.equal(runs(argvLog), 1);
});

test("ls, git status and a cat of a .md file start no check", async (t) => {
  const w = repoWithWorktree(t);
  const argvLog = path.join(w.base, "argv.log");
  for (const command of ["ls src", "git status --short", "cat NOTES.md", "grep -rn backfill src"]) {
    const run = await runHook("post-bash.ts", {
      payload: bashPayload(w.worktree, command, "sess-b2"),
      env: fakeEnv({ HOME: w.home, CARRICK_FAKE_ARGV_LOG: argvLog, CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json") }),
    });
    assert.equal(run.code, 0);
    assert.equal(run.stdout, "");
  }
  assert.equal(runs(argvLog), 0);
});

test("a file read with Read and then with sed is answered once", async (t) => {
  const w = repoWithWorktree(t);
  const argvLog = path.join(w.base, "argv.log");
  const env = fakeEnv({ HOME: w.home, CARRICK_FAKE_ARGV_LOG: argvLog, CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json") });
  const read = await runHook("post-read.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Read",
      session_id: "sess-b3",
      cwd: w.worktree,
      tool_input: { file_path: w.file },
    },
    env,
  });
  assert.ok(contextOf(read.stdout).includes(USES_LINE));
  const sed = await runHook("post-bash.ts", {
    payload: bashPayload(w.worktree, "sed -n 1,20p src/capture/backfill.ts", "sess-b3"),
    env,
  });
  assert.equal(sed.stdout, "");
  assert.equal(runs(argvLog), 1);
});

test("a heredoc write to a TS file is not a read", async (t) => {
  const w = repoWithWorktree(t);
  const argvLog = path.join(w.base, "argv.log");
  const command = "cat > src/capture/backfill.ts <<'EOF'\nexport const b = 2;\nEOF";
  await runHook("post-bash.ts", {
    payload: bashPayload(w.worktree, command, "sess-b4"),
    env: fakeEnv({ HOME: w.home, CARRICK_FAKE_ARGV_LOG: argvLog, CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json") }),
  });
  // The write is the other branch's: one probe, and no read check of the same file.
  assert.equal(runs(argvLog), 1);
  assert.ok(fs.readFileSync(argvLog, "utf8").includes("--no-rescan"));
});

test("a command that reads three TS files asks about the first two", async (t) => {
  const w = repoWithWorktree(t);
  for (const name of ["b.ts", "c.ts", "d.ts"]) fs.writeFileSync(path.join(w.worktree, name), "export {};\n");
  const argvLog = path.join(w.base, "argv.log");
  await runHook("post-bash.ts", {
    payload: bashPayload(w.worktree, "cat b.ts c.ts d.ts", "sess-b5"),
    env: fakeEnv({ HOME: w.home, CARRICK_FAKE_ARGV_LOG: argvLog, CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json") }),
  });
  assert.equal(runs(argvLog), 2);
});

test("bashTargets reads the shapes the agents use and skips writes", (t) => {
  const w = repoWithWorktree(t);
  const f = w.file;
  const at = (command: string, cwd = w.worktree) => bashTargets(command, cwd);
  assert.deepEqual(at("head -n 5 src/capture/backfill.ts | grep x").reads, [f]);
  assert.deepEqual(at("cd src/capture && cat backfill.ts; ls").reads, [f]);
  assert.deepEqual(at("grep -n foo src/capture/backfill.ts").reads, [f]);
  assert.deepEqual(at("grep -n backfill.ts src").reads, [], "the pattern is not a file");
  assert.deepEqual(at("awk '{print}' src/capture/backfill.ts").reads, [f]);
  assert.deepEqual(at("git show HEAD:src/capture/backfill.ts", path.join(w.worktree, "src")).reads, [f]);
  assert.deepEqual(at("sed -i s/a/b/ src/capture/backfill.ts"), { reads: [], writes: [f], factWrites: [] });
  assert.deepEqual(at("cat src/capture/backfill.ts > src/capture/backfill.ts"), { reads: [], writes: [f], factWrites: [] });
  assert.deepEqual(at("rg -g '*.ts' foo").reads, []);
  assert.deepEqual(at("cat src/missing.ts").reads, []);
});

test("carrickHooks registers one Bash entry and self-update adds it to an existing install", (t) => {
  const bash = carrickHooks("carrick").PostToolUse!.filter((group) => group.matcher === "Bash");
  assert.equal(bash.length, 1);
  assert.equal(bash[0]!.hooks[0]!.command, "carrick hook post-bash");

  const base = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-pb-su-"));
  t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  fs.mkdirSync(path.join(base, ".claude"), { recursive: true });
  const without = carrickHooks("carrick");
  without.PostToolUse = without.PostToolUse!.filter((group) => group.matcher !== "Bash");
  const target = path.join(base, ".claude", "settings.local.json");
  fs.writeFileSync(target, JSON.stringify({ hooks: without }, null, 2));
  const changed = updateHooks(base, base, "9.9.9");
  assert.ok(changed.length > 0);
  const after = fs.readFileSync(target, "utf8");
  assert.ok(after.includes("carrick hook post-bash"), after);
  assert.equal(typeof bringUpToDate, "function");
});

test("the search entry carries Grep, Glob and MCP calls, and self-update adds it to an existing install", (t) => {
  const search = carrickHooks("carrick").PostToolUse!.filter((group) => group.matcher === "Grep|Glob|mcp__.*");
  assert.equal(search.length, 1);
  assert.equal(search[0]!.hooks[0]!.command, "carrick hook post-search");

  const base = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-ps-su-"));
  t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  fs.mkdirSync(path.join(base, ".claude"), { recursive: true });
  const without = carrickHooks("carrick");
  without.PostToolUse = without.PostToolUse!.filter((group) => group.matcher !== "Grep|Glob|mcp__.*");
  const target = path.join(base, ".claude", "settings.local.json");
  fs.writeFileSync(target, JSON.stringify({ hooks: without }, null, 2));
  assert.ok(updateHooks(base, base, "9.9.9").length > 0);
  assert.ok(fs.readFileSync(target, "utf8").includes("carrick hook post-search"));
});

// A shell write of a TypeScript file goes through the post-edit flow.

function writeEnv(w: { base: string; home: string }, extra: Record<string, string> = {}) {
  return fakeEnv({
    HOME: w.home,
    CARRICK_FAKE_ARGV_LOG: path.join(w.base, "argv.log"),
    CARRICK_FAKE_FIXTURE: fixturePath("check-counterpart.json"),
    ...extra,
  });
}

function checkedFiles(argvLog: string): string[] {
  if (!fs.existsSync(argvLog)) return [];
  return fs
    .readFileSync(argvLog, "utf8")
    .split("\n")
    .filter((line) => line.includes("--no-rescan"))
    .map((line) => (JSON.parse(line) as { argv: string[] }).argv.find((arg) => arg.endsWith(".ts")) ?? "");
}

test("a heredoc that creates a TS file runs the post-edit flow for it", async (t) => {
  const w = repoWithWorktree(t);
  fs.writeFileSync(path.join(w.worktree, "src", "argv.ts"), "export const flag = 1;\n");
  const command = "cat > src/argv.ts <<'EOF'\nexport const flag = 1;\nEOF";
  const run = await runHook("post-bash.ts", {
    payload: bashPayload(w.worktree, command, "sess-w1"),
    env: writeEnv(w),
  });
  assert.equal(run.code, 0);
  assert.deepEqual(checkedFiles(path.join(w.base, "argv.log")), [".claude/worktrees/wt/src/argv.ts"]);
  assert.match(contextOf(run.stdout), /^Carrick re-checked .* after this edit: 1 contract problem\./);
});

test("a heredoc write of a TS file whose check is clean prints nothing", async (t) => {
  const w = repoWithWorktree(t);
  fs.writeFileSync(path.join(w.worktree, "src", "argv.ts"), "export const flag = 1;\n");
  const command = "cat > src/argv.ts <<'EOF'\nexport const flag = 1;\nEOF";
  const run = await runHook("post-bash.ts", {
    payload: bashPayload(w.worktree, command, "sess-w-clean"),
    env: writeEnv(w, { CARRICK_FAKE_FIXTURE: fixturePath("check-clean.json") }),
  });
  assert.equal(run.code, 0);
  assert.deepEqual(checkedFiles(path.join(w.base, "argv.log")), [".claude/worktrees/wt/src/argv.ts"]);
  assert.equal(run.stdout, "");
});

test("a python heredoc rewrite of a TS file runs the post-edit flow once for it", async (t) => {
  const w = repoWithWorktree(t);
  const command = "python3 - <<'EOF'\nimport pathlib\np = pathlib.Path('src/capture/backfill.ts')\np.write_text('export const b = 2;\\n')\nEOF";
  fs.writeFileSync(w.file, "export const b = 2;\n");
  const run = await runHook("post-bash.ts", {
    payload: bashPayload(w.worktree, command, "sess-w2"),
    env: writeEnv(w),
  });
  assert.equal(run.code, 0);
  assert.deepEqual(checkedFiles(path.join(w.base, "argv.log")), [".claude/worktrees/wt/src/capture/backfill.ts"]);
});

test("a git checkout that changes many files starts nothing", async (t) => {
  const w = repoWithWorktree(t);
  fs.writeFileSync(w.file, "export const b = 3;\n");
  const run = await runHook("post-bash.ts", {
    payload: bashPayload(w.worktree, "git checkout -q main && sed -i s/a/b/ src/capture/backfill.ts", "sess-w3"),
    env: writeEnv(w),
  });
  assert.equal(run.stdout, "");
  assert.deepEqual(checkedFiles(path.join(w.base, "argv.log")), []);
});

test("a command that names a TS file it did not change starts nothing", async (t) => {
  const w = repoWithWorktree(t);
  const env = writeEnv(w);
  // No record, and the file is old: nothing.
  const old = new Date(Date.now() - 3_600_000);
  fs.utimesSync(w.file, old, old);
  const first = await runHook("post-bash.ts", {
    payload: bashPayload(w.worktree, "python3 -c \"print(open('src/capture/backfill.ts').read())\"", "sess-w4"),
    env,
  });
  assert.equal(first.stdout, "");
  assert.deepEqual(checkedFiles(path.join(w.base, "argv.log")), []);
  // A write is seen once; the same command again with the file as it was is not a second write.
  fs.writeFileSync(w.file, "export const b = 4;\n");
  const write = "sed -i s/4/5/ src/capture/backfill.ts";
  await runHook("post-bash.ts", { payload: bashPayload(w.worktree, write, "sess-w4"), env });
  await runHook("post-bash.ts", { payload: bashPayload(w.worktree, write, "sess-w4"), env });
  assert.deepEqual(checkedFiles(path.join(w.base, "argv.log")), [".claude/worktrees/wt/src/capture/backfill.ts"]);
});

test("an Edit and then a Bash write of the same file keep one queue entry each, in order", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-pb-q-")));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const env = fakeEnv({
    HOME: home,
    CARRICK_FAKE_PROBE_FIXTURE: fixturePath("check-deferred.json"),
    CARRICK_FAKE_FIXTURE: fixturePath("check-counterpart.json"),
  });
  const session = "sess-w5";
  await runHook("post-edit.ts", { payload: { ...editPayload(workspace), session_id: session }, env });
  await runHook("post-bash.ts", {
    payload: bashPayload(workspace.root, `sed -i s/a/b/ ${workspace.file}`, session),
    env,
  });
  const answers = () =>
    fs
      .readdirSync(path.join(home, ".carrick", "sessions"))
      .filter((name) => name.startsWith(`${session}.recheck.`) && name.endsWith(".answer.json"))
      .sort();
  const until = Date.now() + 20_000;
  while (Date.now() < until && answers().length < 2) await new Promise((resolve) => setTimeout(resolve, 50));
  const done = answers();
  assert.equal(done.length, 2, "one answered entry per write");
  assert.ok(done[0]! < done[1]!, "in order");
});

test("bashTargets reads the write shapes the agents use", (t) => {
  const w = repoWithWorktree(t);
  const f = w.file;
  const g = path.join(w.worktree, "src", "g.ts");
  fs.writeFileSync(g, "export {};\n");
  const writes = (command: string) => bashTargets(command, w.worktree).writes;
  assert.deepEqual(writes("cat > src/capture/backfill.ts <<'EOF'\nx\nEOF"), [f]);
  assert.deepEqual(writes("cat <<'EOF' > src/capture/backfill.ts\nx\nEOF"), [f]);
  assert.deepEqual(writes("echo hi | tee src/capture/backfill.ts"), [f]);
  assert.deepEqual(writes("cp /tmp/x src/g.ts"), [g]);
  assert.deepEqual(writes("node - <<EOF\nrequire('fs').writeFileSync('src/g.ts', 'x')\nEOF"), [g]);
  assert.deepEqual(
    writes("apply_patch <<'EOF'\n*** Begin Patch\n*** Update File: src/g.ts\n@@\n-a\n+b\n*** End Patch\nEOF"),
    [g],
  );
  // Text after a heredoc body is still commands, and the body is not.
  assert.deepEqual(bashTargets("cat <<EOF\ncat src/g.ts\nEOF\nsed -n 1,5p src/g.ts", w.worktree).reads, [g]);
  assert.ok(movesTreeInBulk("git -C x checkout main", w.worktree));
  assert.ok(movesTreeInBulk("cd a && npm ci", w.worktree));
  assert.ok(!movesTreeInBulk("git diff && npm test", w.worktree));
});

// The payload's `cwd` is the directory after the command when a `cd` persisted (#4425).

/** A tree `<root>/a/src/x.ts` for the shell-write-after-cd tests. */
function cdTree(t: { after: (fn: () => void) => void }) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-cd-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, "a", "src"), { recursive: true });
  const x = path.join(root, "a", "src", "x.ts");
  fs.writeFileSync(x, "export {};\n");
  return { root, a: path.join(root, "a"), x };
}

test("a write after a cd is found whether cwd is the directory before or after the command", (t) => {
  const d = cdTree(t);
  const heredoc = "cd a; cat > src/x.ts <<'EOF'\nexport const y = 1;\nEOF";
  assert.deepEqual(bashTargets("cd a && echo hi > src/x.ts", d.a).writes, [d.x], "after directory");
  assert.deepEqual(bashTargets("cd a && echo hi > src/x.ts", d.root).writes, [d.x], "start directory");
  assert.deepEqual(bashTargets(heredoc, d.a).writes, [d.x], "heredoc, after directory");
  assert.deepEqual(bashTargets(heredoc, d.root).writes, [d.x], "heredoc, start directory");
  assert.deepEqual(bashTargets("(cd a && echo hi > src/x.ts)", d.root).writes, [d.x], "subshell");
  assert.deepEqual(bashTargets("cd a && cat src/x.ts", d.a).reads, [d.x], "a read after the cd");
  assert.deepEqual(searchedNames("cd a && grep -n widgetName src/x.ts", d.a).operands, [d.x]);
});

test("a write with no cd resolves as before", (t) => {
  const d = cdTree(t);
  assert.deepEqual(bashTargets("echo hi > a/src/x.ts", d.root).writes, [d.x]);
  assert.deepEqual(bashTargets("echo hi > src/x.ts", d.a).writes, [d.x]);
  assert.deepEqual(bashTargets("echo hi > src/x.ts", d.root).writes, []);
});

test("a file present under both readings: the one changed in the call's window wins", (t) => {
  const d = cdTree(t);
  // `<root>/a/a/src/x.ts` exists too: cwd = <root>/a is read as after (a/src/x.ts... start <root>) and as start.
  const nested = path.join(d.a, "a", "src", "x.ts");
  fs.mkdirSync(path.dirname(nested), { recursive: true });
  fs.writeFileSync(nested, "export {};\n");
  const old = new Date(Date.now() - 3_600_000);
  const command = "cd a && echo hi > src/x.ts";
  fs.utimesSync(nested, old, old);
  assert.deepEqual(bashTargets(command, d.a, { durationMs: 500 }).writes, [d.x], "only the after-reading file changed");
  fs.utimesSync(nested, new Date(), new Date());
  fs.utimesSync(d.x, old, old);
  assert.deepEqual(bashTargets(command, d.a, { durationMs: 500 }).writes, [nested], "only the start-reading file changed");
  fs.utimesSync(d.x, new Date(), new Date());
  assert.deepEqual(bashTargets(command, d.a, { durationMs: 500 }).writes.sort(), [d.x, nested].sort(), "both changed: both kept");
});

test("a shell write after a cd, with the after directory, gets its after-write check", async (t) => {
  const w = repoWithWorktree(t);
  const sub = path.join(w.worktree, "src");
  fs.writeFileSync(w.file, "export const b = 2;\n");
  const run = await runHook("post-bash.ts", {
    payload: bashPayload(sub, "cd src && echo 'export const b = 2;' > capture/backfill.ts", "sess-cd1"),
    env: writeEnv(w),
  });
  assert.equal(run.code, 0);
  assert.deepEqual(checkedFiles(path.join(w.base, "argv.log")), [".claude/worktrees/wt/src/capture/backfill.ts"]);
});

// A grep for an indexed function names the sites its output did not print.

/** A workspace whose index holds `readWidget` (defined in lib.ts) called from a.ts, b.ts and c.ts. */
function indexedWorkspace(t: { after: (fn: () => void) => void }) {
  const w = repoWithWorktree(t);
  const root = w.repo;
  const site = (file: string, line: number) => ({ service: "svc", file, line, function: "run" });
  fs.writeFileSync(
    path.join(root, ".carrick", "index.json"),
    JSON.stringify({
      repos: [
        {
          path: root,
          name: "svc",
          uses: {
            "src/lib.ts": {
              functions: [
                { name: "readWidget", line: 4, callers_total: 3, callers: [site("src/a.ts", 3), site("src/b.ts", 9), site("src/c.ts", 2)] },
                { name: "common", line: 1, callers_total: 1, callers: [site("src/a.ts", 1)] },
                { name: "common", line: 1, callers_total: 1, callers: [site("src/b.ts", 1)] },
              ],
            },
          },
        },
      ],
    }),
  );
  return { ...w, root };
}

function grepRun(w: { root: string; home: string }, command: string, stdout: string, session: string, cwd = w.root) {
  return runHook("post-bash.ts", {
    payload: { ...bashPayload(cwd, command, session), tool_response: { stdout } },
    env: fakeEnv({ HOME: w.home }),
  });
}

test("a grep that printed every caller prints nothing", async (t) => {
  const w = indexedWorkspace(t);
  const out = "src/lib.ts:4:export function readWidget\nsrc/a.ts:3:readWidget()\nsrc/b.ts:9:readWidget()\nsrc/c.ts:2:readWidget()\n";
  const run = await grepRun(w, "grep -rn readWidget src", out, "sess-g1");
  assert.equal(run.code, 0);
  assert.equal(run.stdout, "");
});

test("a grep scoped to one folder names the caller outside it", async (t) => {
  const w = indexedWorkspace(t);
  const out = "src/lib.ts:4:export function readWidget\nsrc/a.ts:3:readWidget()\n";
  const run = await grepRun(w, "grep -rn 'function readWidget\\|readWidget(' src/a.ts src/lib.ts", out, "sess-g2");
  const context = contextOf(run.stdout);
  assert.equal(
    context,
    "Carrick: `readWidget` (src/lib.ts:4) is also called where this search did not reach: src/b.ts:9, src/c.ts:2.",
  );
  // Once per name per session.
  const again = await grepRun(w, "rg readWidget src/a.ts", out, "sess-g2");
  assert.equal(again.stdout, "");
});

test("a token the map does not hold prints nothing", async (t) => {
  const w = indexedWorkspace(t);
  for (const command of ["grep -rn otherThing src", "grep -rn common src", "grep -rn 'read widget' src", "ls | grep readWidget"]) {
    const run = await grepRun(w, command, "", "sess-g3");
    assert.equal(run.stdout, "", command);
  }
});

test("a grep run from a sub-folder matches its output paths against the folder", async (t) => {
  const w = indexedWorkspace(t);
  const out = "lib.ts:4:readWidget\na.ts:3:readWidget\nb.ts:9:readWidget\nc.ts:2:readWidget\n";
  const run = await grepRun(w, "grep -n readWidget *.ts", out, "sess-g4", path.join(w.root, "src"));
  assert.equal(run.stdout, "");
});

// The Bash hook writes the timing row the edit hook writes (carrick#4415).

function timingRows(root: string) {
  const file = path.join(root, ".carrick", "hook-timings.jsonl");
  if (!fs.existsSync(file)) return [];
  return fs
    .readFileSync(file, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line) as Record<string, unknown>);
}

test("a shell write of a TS file writes one post-bash timing row with waited_ms and the file", async (t) => {
  const w = repoWithWorktree(t);
  fs.writeFileSync(path.join(w.worktree, "src", "argv.ts"), "export const flag = 1;\n");
  const command = "cat > src/argv.ts <<'EOF'\nexport const flag = 1;\nEOF";
  const run = await runHook("post-bash.ts", {
    payload: bashPayload(w.worktree, command, "sess-timing-w"),
    cwd: w.worktree,
    env: writeEnv(w),
  });
  assert.equal(run.code, 0);
  const rows = timingRows(w.repo).filter((r) => r["hook"] === "post-bash");
  assert.equal(rows.length, 1);
  assert.equal(typeof rows[0]!["waited_ms"], "number");
  assert.equal(typeof rows[0]!["duration_ms"], "number");
  assert.ok(["printed", "silent"].includes(rows[0]!["outcome"] as string));
  assert.equal(rows[0]!["file"], ".claude/worktrees/wt/src/argv.ts");
  assert.equal(typeof rows[0]!["facts_ms"], "number");
});

test("a shell command that touches no file writes one silent post-bash row, as the edit hook does for nothing to do", async (t) => {
  const w = repoWithWorktree(t);
  const run = await runHook("post-bash.ts", {
    payload: bashPayload(w.worktree, "ls -la", "sess-timing-none"),
    cwd: w.worktree,
    env: writeEnv(w),
  });
  assert.equal(run.code, 0);
  const rows = timingRows(w.repo).filter((r) => r["hook"] === "post-bash");
  assert.equal(rows.length, 1);
  assert.equal(rows[0]!["outcome"], "silent");
  assert.equal(rows[0]!["waited_ms"], 0);
  assert.equal("file" in rows[0]!, false);
});
