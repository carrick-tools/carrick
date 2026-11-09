// The Bash hook (carrick hook post-bash): a shell command that reads a
// TypeScript file gets the Read hook's answer.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { bashTargets } from "../src/hook/bash-targets.ts";
import { bringUpToDate, updateHooks } from "../src/init/self-update.ts";
import { carrickHooks } from "../src/init/settings.ts";
import { fakeEnv, fixturePath, runHook } from "./helpers.ts";

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
  const run = await runHook("post-bash.ts", {
    payload: bashPayload(w.worktree, command, "sess-b4"),
    env: fakeEnv({ HOME: w.home, CARRICK_FAKE_ARGV_LOG: argvLog, CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json") }),
  });
  assert.equal(run.stdout, "");
  assert.equal(runs(argvLog), 0);
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
  assert.deepEqual(at("sed -i s/a/b/ src/capture/backfill.ts"), { reads: [], writes: [f] });
  assert.deepEqual(at("cat src/capture/backfill.ts > src/capture/backfill.ts"), { reads: [], writes: [f] });
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
