// The Read hook reaches an agent whose cwd is a linked git worktree of an
// indexed repo (carrick#3672 T8): the shape our own agents use,
// <repo>/.claude/worktrees/<name>. The index lives with the repo (`.carrick/`
// is not committed, so a worktree does not have it); the caller lines must
// still arrive, and the check must name the worktree's file.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fakeEnv, firstCall, fixturePath, runHook } from "./helpers.ts";

const USES_LINE =
  "- CatalogClient.readWidget (line 12) has 2 caller(s) outside this file: src/inventory.ts:9, src/inventory.ts:17";

function git(dir: string, ...args: string[]): void {
  execFileSync("git", args, {
    cwd: dir,
    stdio: "ignore",
    env: { ...process.env, GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t" },
  });
}

/** An indexed repo (with `.carrick/`) and a linked worktree of it at `.claude/worktrees/wt`. */
function repoWithWorktree(t: { after: (fn: () => void) => void }) {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-t8-")));
  t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  const home = path.join(base, "home");
  fs.mkdirSync(home);
  const repo = path.join(base, "inventory-svc");
  fs.mkdirSync(path.join(repo, "src"), { recursive: true });
  fs.writeFileSync(path.join(repo, "src", "client.ts"), "export const a = 1;\n");
  fs.writeFileSync(path.join(repo, ".gitignore"), ".carrick/\n.claude/worktrees/\n");
  git(repo, "init", "-q", "-b", "main");
  git(repo, "add", ".");
  git(repo, "commit", "-q", "-m", "init");
  fs.mkdirSync(path.join(repo, ".carrick"));
  git(repo, "worktree", "add", "-q", ".claude/worktrees/wt", "-b", "wt");
  const worktree = path.join(repo, ".claude", "worktrees", "wt");
  return { base, home, repo, worktree, file: path.join(worktree, "src", "client.ts") };
}

function payload(cwd: string, file: string, session: string) {
  return {
    hook_event_name: "PostToolUse",
    tool_name: "Read",
    session_id: session,
    cwd,
    tool_input: { file_path: file },
    tool_response: { file: { filePath: file, content: "export const a = 1;" } },
  };
}

function contextOf(stdout: string): string {
  if (!stdout) return "";
  return (JSON.parse(stdout) as { hookSpecificOutput: { additionalContext: string } })
    .hookSpecificOutput.additionalContext;
}

for (const [label, projectDir] of [
  ["no project dir", false],
  ["project dir is the repo", true],
] as const) {
  test(`a Read in a git worktree of an indexed repo prints the caller lines (${label})`, async (t) => {
    const w = repoWithWorktree(t);
    const argvLog = path.join(w.base, "argv.log");
    const run = await runHook("post-read.ts", {
      payload: payload(w.worktree, w.file, `sess-t8-${projectDir}`),
      env: fakeEnv({
        HOME: w.home,
        ...(projectDir ? { CLAUDE_PROJECT_DIR: w.repo } : {}),
        CARRICK_FAKE_ARGV_LOG: argvLog,
        CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json"),
      }),
    });
    assert.equal(run.code, 0);
    assert.ok(contextOf(run.stdout).includes(USES_LINE), `stdout: ${run.stdout}\nstderr: ${run.stderr}`);
    const call = firstCall(argvLog);
    assert.ok(call);
    // The check runs where the index is (the repo, which holds `.carrick/`) and
    // names the worktree's file by a path that resolves to it; the binary places
    // that file as the repo's own (src/local_mode/read_model.rs `locate_file`,
    // carrick#3595).
    assert.equal(fs.realpathSync(call.cwd), w.repo);
    const named = call.argv.find((arg) => arg.endsWith("client.ts"));
    assert.ok(named, call.argv.join(" "));
    assert.equal(fs.realpathSync(path.resolve(call.cwd, named)), fs.realpathSync(w.file));
  });
}
