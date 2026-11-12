// Helpers shared by the fact-line tests (`facts-kinds.test.ts`, `facts-cap.test.ts`):
// the hand-written fact file, a workspace built from it, an Edit hook run, and the
// row builders. Moved out of `facts-kinds.test.ts` unchanged.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { drainRunner, fakeEnv, fixturePath, runHook, waitForSessionAnswers } from "./helpers.ts";
import { commitRepos } from "./facts-repos.ts";
import { buildFacts, factsPath } from "../src/facts/store.ts";

export const SLUG = "kinds-demo";
/** A peer in the mapped, committed `api` repo (`commitRepos` writes the file): an op row needs one (ingest skips `others: []`). */
export const ELSEWHERE = { service: "api", repo: "api", file: "src/peer.ts", line: 1 };
export const REPOS = ["api", "web"];

export type Ws = { root: string; home: string; at: (rel: string) => string };

/** The header; `commits` replaces each repo's commit, and a repo it maps to null is left out. */
export const header = (commits: Record<string, string | null> = {}) =>
  JSON.stringify({
    schema: 1,
    writer: 1,
    project_id: "p",
    project_slug: SLUG,
    digest: "d1",
    built_at: "2026-01-01T00:00:00Z",
    repos: REPOS.filter((repo) => commits[repo] !== null).map((repo) => ({ repo, service: repo, commit: commits[repo] ?? "1" })),
    counts: { rows: 0, producers: 0, consumers: 0 },
  });

export const ndjson = (rows: Array<Record<string, unknown>>, commits: Record<string, string | null> = {}) =>
  `${[header(commits), ...rows.map((row) => JSON.stringify(row))].join("\n")}\n`;

/** A workspace with `files` written and, unless `rows` is null, a fact file built from them. */
export async function workspace(
  t: { after: (fn: () => void) => void },
  files: Record<string, string>,
  rows: Array<Record<string, unknown>> | null,
): Promise<Ws> {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-kinds-")));
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-kinds-home-")));
  t.after(async () => {
    // An edit with a fact file queues its check: no runner outlives the test.
    await drainRunner(root);
    fs.rmSync(root, { recursive: true, force: true });
    fs.rmSync(home, { recursive: true, force: true });
  });
  for (const [rel, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), text);
  }
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: SLUG }] } }));
  if (rows !== null) {
    // Only a repo folder the test wrote is a checkout here, as `matchRepos`
    // maps only what it finds; each is committed and the header names it.
    const local = REPOS.filter((repo) => fs.existsSync(path.join(root, repo)));
    await buildFacts(commitRepos(root, ndjson(rows), local), factsPath(root), local.map((repo) => ({ repo, path: path.join(root, repo) })), new Date().toISOString());
  }
  return { root, home, at: (rel) => path.join(root, rel) };
}

export type Hunk = { newStart: number; lines: string[] };

/** Run post-edit for an Edit of `file` whose result carries `hunks`. */
export function patchEdit(ws: Ws, file: string, hunks: Hunk[], options: { session?: string; probe?: string; argvLog?: string; env?: Record<string, string> } = {}) {
  return runHook("post-edit.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      session_id: options.session ?? "kinds",
      cwd: ws.root,
      tool_input: { file_path: file, old_string: "x", new_string: "" },
      tool_response: { structuredPatch: hunks.map((hunk) => ({ oldStart: hunk.newStart, oldLines: 0, newLines: 0, ...hunk })) },
    },
    env: fakeEnv({
      HOME: ws.home,
      CLAUDE_PROJECT_DIR: ws.root,
      CARRICK_FAKE_FIXTURE: fixturePath(options.probe ?? "check-silent.json"),
      ...(options.argvLog ? { CARRICK_FAKE_ARGV_LOG: options.argvLog } : {}),
      ...options.env,
    }),
  });
}

export function context(stdout: string): string {
  if (!stdout.trim()) return "";
  return (JSON.parse(stdout) as { hookSpecificOutput: { additionalContext: string } }).hookSpecificOutput.additionalContext;
}

/**
 * With a fact file an edit's check is queued in place of the probe (T12-2):
 * wait for the runner's answer, then what the next carrier (a shell command
 * that writes nothing) prints for `session`.
 */
export async function nextCarrier(ws: Ws, session: string, fixture = "check-uses.json"): Promise<string> {
  await waitForSessionAnswers(ws.home, session);
  const run = await runHook("post-bash.ts", {
    payload: { hook_event_name: "PostToolUse", tool_name: "Bash", session_id: session, cwd: ws.root, tool_input: { command: "ls" }, tool_response: { stdout: "" } },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath(fixture) }),
  });
  return context(run.stdout);
}

/** The 1-based line of `file` holding `needle`. */
export function lineOf(file: string, needle: string): number {
  return fs.readFileSync(file, "utf8").split("\n").findIndex((line) => line.includes(needle)) + 1;
}

export const fnRow = (name: string, text: string, file = "src/orders.ts") => ({
  k: "fn",
  repo: "api",
  file,
  line: 1,
  name,
  // A peer in the mapped, committed `api` repo (`commitRepos` writes the file); the text is the row's own.
  others: [{ repo: "api", file: "src/caller.ts", line: 4, name: "render" }],
  total: 1,
  text,
});

export const nameRow = (name: string, repo: string, file: string) => ({
  k: "name",
  name,
  repo,
  file,
  line: 3,
  text: `a function named ${name} is declared at ${repo}/${file}:3`,
});

/** A git on PATH that logs every call to `log`, then runs the real one. */
export function countingGit(t: { after: (fn: () => void) => void }): { path: string; calls: (pattern: RegExp) => number } {
  const shim = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-git-log-"));
  t.after(() => fs.rmSync(shim, { recursive: true, force: true }));
  const log = path.join(shim, "calls.log");
  const real = spawnSync("sh", ["-c", "command -v git"], { encoding: "utf8" }).stdout.trim();
  fs.writeFileSync(path.join(shim, "git"), `#!/bin/sh\necho "$*" >> ${log}\nexec ${real} "$@"\n`, { mode: 0o755 });
  return {
    path: `${shim}${path.delimiter}${process.env["PATH"] ?? ""}`,
    calls: (pattern) => (fs.existsSync(log) ? fs.readFileSync(log, "utf8").split("\n").filter((line) => pattern.test(line)).length : 0),
  };
}
