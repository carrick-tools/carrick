// Shared test scaffolding: fixture payloads, a fake `carrick` on PATH, and a
// throwaway workspace with a `.carrick/` marker in it.

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  parseCheckResult,
  parseStatusResult,
  type CheckResult,
  type StatusResult,
} from "../src/contract.ts";

// The fact lines are on unless CARRICK_FACT_LINES is "0" (`src/facts/enabled.ts`).
// This file pins the switch to "1" for the test process and, through `fakeEnv`,
// for every hook child. An off test sets "0"; a default test deletes it.
process.env["CARRICK_FACT_LINES"] = "1";
// A test that expects a fact line must not lose it to a slow git call (the
// diff against HEAD, the peer proof) on a busy machine: in this process and,
// through `fakeEnv`, in every hook child. A test of the budget itself passes
// its own value.
process.env["CARRICK_FACTS_DIFF_BUDGET_MS"] = "5000";

export const testDir =path.dirname(fileURLToPath(import.meta.url));
export const pluginDir = path.dirname(testDir);
export const fakeBin = path.join(testDir, "fake-carrick.mjs");

export function fixturePath(name: string): string {
  return path.join(testDir, "fixtures", name);
}

export function fixture(name: string): CheckResult {
  const parsed = parseCheckResult(fs.readFileSync(fixturePath(name), "utf8"));
  if (!parsed) throw new Error(`fixture ${name} is not a carrick.check/0 payload`);
  return parsed;
}

export function statusFixture(name: string): StatusResult {
  const parsed = parseStatusResult(fs.readFileSync(fixturePath(name), "utf8"));
  if (!parsed) throw new Error(`fixture ${name} is not a carrick.status/0 payload`);
  return parsed;
}

export type Workspace = {
  root: string;
  /** A service directory inside the workspace, with no marker of its own. */
  service: string;
  /** A `.ts` file inside that service. */
  file: string;
  cleanup: () => void;
};

/**
 * A file with real, indented lines at the line numbers the fixtures name.
 *
 * The span a diagnostic underlines is the text of its line (carrick#922), so a
 * one-line stand-in would leave every row on the fallback range and prove
 * nothing. Every line is indented and every line is a different length, which
 * is what makes a widened span readable in an assertion.
 */
export function sourceText(lines: number): string {
  const body = [`import { thing } from "./thing";`];
  for (let line = 2; line <= lines; line += 1) {
    body.push(`  const value${line} = thing(${"x".repeat(line % 7)});`);
  }
  return `${body.join("\n")}\n`;
}

/**
 * A workspace laid out the way the local mode expects: `.carrick/` at the top,
 * service directories under it, and no marker inside a service.
 *
 * The counterpart files the fixtures name exist here too, because a counterpart
 * path is relative to its own repo and only resolves when that repo is on disk.
 */
export function makeWorkspace(): Workspace {
  // Resolved, because on macOS `os.tmpdir()` is a symlink and a workspace under
  // it has two spellings: the client opens `/var/...` and a counterpart path
  // the CLI answers with resolves to `/private/var/...`. Two spellings of one
  // file are two files to everything that keys on a path, which is a real
  // hazard in a symlinked workspace and not the one under test here.
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-plugin-")));
  fs.mkdirSync(path.join(root, ".carrick"), { recursive: true });
  const service = path.join(root, "user-service");
  fs.mkdirSync(path.join(service, "src", "routes"), { recursive: true });
  const file = path.join(service, "src", "routes", "users.ts");
  fs.writeFileSync(file, sourceText(130));
  for (const counterpart of [
    "order-service/src/clients/users.ts",
    "order-service/src/server.ts",
    "billing-service/src/lookup.ts",
    "billing-service/src/charges.ts",
  ]) {
    const target = path.join(root, counterpart);
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.writeFileSync(target, sourceText(130));
  }
  return {
    root,
    service,
    file,
    cleanup: () => fs.rmSync(root, { recursive: true, force: true }),
  };
}

/**
 * The first complete call the fake CLI logged, or null while it is mid-write.
 *
 * `existsSync` goes true when the file is created, which can be before the line
 * in it is whole, so a test waits on this rather than on the file.
 */
export function firstCall(argvLog: string): { argv: string[]; cwd: string } | null {
  try {
    const line = fs
      .readFileSync(argvLog, "utf8")
      .split("\n")
      .find((candidate) => candidate.trim().length > 0);
    return line ? (JSON.parse(line) as { argv: string[]; cwd: string }) : null;
  } catch {
    return null;
  }
}

/**
 * Wait, at most 20 s, until no re-check runner holds the workspace's lock and
 * its queue is empty, so no runner a test started outlives the test's temp
 * folders. Returns whether it drained.
 */
export async function drainRunner(root: string): Promise<boolean> {
  const rechecks = path.join(root, ".carrick", "rechecks");
  const queued = () => {
    try {
      return fs.readdirSync(path.join(rechecks, "queue")).length;
    } catch {
      return 0;
    }
  };
  for (let attempt = 0; attempt < 400; attempt += 1) {
    if (!fs.existsSync(path.join(rechecks, "runner.lock")) && queued() === 0) return true;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  return false;
}

/**
 * A counted wait (at most 50 x 100 ms) until the runner has written `count`
 * answers for `session` under `home`. Throws when it has not.
 */
export async function waitForSessionAnswers(home: string, session: string, count = 1): Promise<void> {
  const sessions = path.join(home, ".carrick", "sessions");
  let found = 0;
  for (let attempt = 0; attempt < 50; attempt += 1) {
    const names = fs.existsSync(sessions) ? fs.readdirSync(sessions) : [];
    found = names.filter((name) => name.startsWith(`${session}.recheck.`) && name.endsWith(".answer.json")).length;
    if (found >= count) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`the runner wrote ${found} of ${count} answer(s) for ${session}`);
}

export type HookRun = { stdout: string; stderr: string; code: number | null; ms: number };

/** Run one of the hook scripts the way Claude Code runs it: payload on stdin. */
export function runHook(
  script: string,
  options: { payload?: unknown; env?: NodeJS.ProcessEnv; cwd?: string; nodeArgs?: string[] },
): Promise<HookRun> {
  const started = Date.now();
  return new Promise((resolve) => {
    const args = [...(options.nodeArgs ?? []), path.join(pluginDir, "src", "hook", script)];
    const child = spawn(process.execPath, args, {
      env: options.env ?? fakeEnv(),
      cwd: options.cwd ?? pluginDir,
      stdio: ["pipe", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk: Buffer) => (stdout += chunk.toString("utf8")));
    child.stderr.on("data", (chunk: Buffer) => (stderr += chunk.toString("utf8")));
    child.on("close", (code) => resolve({ stdout, stderr, code, ms: Date.now() - started }));
    child.stdin.end(JSON.stringify(options.payload ?? {}));
  });
}

/** The PostToolUse payload Claude Code sends after an Edit. */
export function editPayload(workspace: { root: string; file: string }, tool = "Edit") {
  return {
    hook_event_name: "PostToolUse",
    tool_name: tool,
    cwd: workspace.root,
    tool_input: { file_path: workspace.file },
  };
}

/** Env that points the plugin at the fake CLI and keeps its logs off stderr. */
export function fakeEnv(overrides: Record<string, string> = {}): NodeJS.ProcessEnv {
  return {
    ...process.env,
    CARRICK_BIN: fakeBin,
    CARRICK_LOG_QUIET: "1",
    CARRICK_FAKE_FIXTURE: fixturePath("check-mismatch.json"),
    // The fixtures speak in absolute paths under /workspace; an integration
    // test's workspace is a fresh temp directory, so the fake CLI rebases them.
    CARRICK_FAKE_REBASE: "1",
    ...overrides,
  };
}
