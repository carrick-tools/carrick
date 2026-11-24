// An agent in a linked git worktree is told once where the rules are (carrick#5134):
// `.carrick/RULES.md` is not tracked, so the worktree has the AGENTS.md line and no file.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fakeEnv, fixturePath, runHook } from "./helpers.ts";

function git(dir: string, ...args: string[]): void {
  execFileSync("git", args, {
    cwd: dir,
    stdio: "ignore",
    env: { ...process.env, GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t" },
  });
}

function setup(t: { after: (fn: () => void) => void }, options: { rules: boolean }) {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-rules-wt-")));
  t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  const home = path.join(base, "home");
  fs.mkdirSync(home);
  const repo = path.join(base, "svc");
  fs.mkdirSync(path.join(repo, "src"), { recursive: true });
  fs.writeFileSync(path.join(repo, "src", "client.ts"), "export const a = 1;\n");
  fs.writeFileSync(path.join(repo, ".gitignore"), ".carrick/\n.claude/worktrees/\n");
  git(repo, "init", "-q", "-b", "main");
  git(repo, "add", ".");
  git(repo, "commit", "-q", "-m", "init");
  fs.mkdirSync(path.join(repo, ".carrick"));
  fs.writeFileSync(path.join(repo, ".carrick", "index.json"), "{}");
  if (options.rules) fs.writeFileSync(path.join(repo, ".carrick", "RULES.md"), "rules\n");
  git(repo, "worktree", "add", "-q", ".claude/worktrees/wt", "-b", "wt");
  const worktree = path.join(repo, ".claude", "worktrees", "wt");
  const env = fakeEnv({ HOME: home, CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json") });
  return { base, repo, worktree, env, rules: path.join(repo, ".carrick", "RULES.md") };
}

const lineFor = (rules: string) => `Carrick: this worktree has no copy of Carrick's rules; read ${rules} instead.`;
const count = (text: string, needle: string) => text.split(needle).length - 1;

function payload(tool: "Read" | "Edit", cwd: string, file: string, session: string) {
  return {
    hook_event_name: "PostToolUse",
    tool_name: tool,
    session_id: session,
    cwd,
    tool_input: { file_path: file },
    tool_response: { file: { filePath: file, content: "export const a = 1;" } },
  };
}

for (const [tool, script] of [["Read", "post-read.ts"], ["Edit", "post-edit.ts"]] as const) {
  test(`a ${tool} in a linked worktree says where the rules are, once`, async (t) => {
    const w = setup(t, { rules: true });
    const file = path.join(w.worktree, "src", "client.ts");
    const first = await runHook(script, { payload: payload(tool, w.worktree, file, `s-${tool}`), env: w.env });
    assert.equal(count(first.stdout, lineFor(w.rules)), 1, `stdout: ${first.stdout}\nstderr: ${first.stderr}`);
    const second = await runHook(script, { payload: payload(tool, w.worktree, file, `s-${tool}`), env: w.env });
    assert.equal(count(second.stdout, lineFor(w.rules)), 0, second.stdout);
    // Nothing is written in the worktree.
    assert.equal(fs.existsSync(path.join(w.worktree, ".carrick")), false);
  });

  test(`a ${tool} in the main checkout never says it`, async (t) => {
    const w = setup(t, { rules: true });
    const file = path.join(w.repo, "src", "client.ts");
    const run = await runHook(script, { payload: payload(tool, w.repo, file, `m-${tool}`), env: w.env });
    assert.equal(count(run.stdout, "no copy of Carrick's rules"), 0, run.stdout);
  });

  test(`a ${tool} in a worktree says nothing when the main checkout has no rules file`, async (t) => {
    const w = setup(t, { rules: false });
    const file = path.join(w.worktree, "src", "client.ts");
    const run = await runHook(script, { payload: payload(tool, w.worktree, file, `n-${tool}`), env: w.env });
    assert.equal(count(run.stdout, "no copy of Carrick's rules"), 0, run.stdout);
  });
}

test("a worktree that holds its own rules file is told nothing", async (t) => {
  const w = setup(t, { rules: true });
  fs.mkdirSync(path.join(w.worktree, ".carrick"));
  fs.writeFileSync(path.join(w.worktree, ".carrick", "RULES.md"), "own\n");
  const file = path.join(w.worktree, "src", "client.ts");
  const run = await runHook("post-read.ts", { payload: payload("Read", w.worktree, file, "own"), env: w.env });
  assert.equal(count(run.stdout, "no copy of Carrick's rules"), 0, run.stdout);
});

test("a session that starts inside a worktree says it once, and the first Read does not repeat it", async (t) => {
  const w = setup(t, { rules: true });
  const start = await runHook("session-start.ts", {
    payload: { hook_event_name: "SessionStart", session_id: "ss" },
    env: { ...w.env, CLAUDE_PROJECT_DIR: w.worktree },
    cwd: w.worktree,
  });
  assert.equal(count(start.stdout, lineFor(w.rules)), 1, `stdout: ${start.stdout}\nstderr: ${start.stderr}`);
  const file = path.join(w.worktree, "src", "client.ts");
  const read = await runHook("post-read.ts", { payload: payload("Read", w.worktree, file, "ss"), env: w.env });
  assert.equal(count(read.stdout, "no copy of Carrick's rules"), 0, read.stdout);
  assert.equal(fs.existsSync(path.join(w.worktree, ".carrick")), false);
});

test("a sub-agent has its own once-per-session line", async (t) => {
  const w = setup(t, { rules: true });
  const file = path.join(w.worktree, "src", "client.ts");
  await runHook("post-read.ts", { payload: payload("Read", w.worktree, file, "p"), env: w.env });
  const sub = await runHook("post-read.ts", { payload: { ...payload("Read", w.worktree, file, "p"), agent_id: "a1" }, env: w.env });
  assert.equal(count(sub.stdout, lineFor(w.rules)), 1, sub.stdout);
});
