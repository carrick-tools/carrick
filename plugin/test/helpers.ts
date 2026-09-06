// Shared test scaffolding: fixture payloads, a fake `carrick` on PATH, and a
// throwaway workspace with a `.carrick/` marker in it.

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseCheckResult, type CheckResult } from "../src/contract.ts";

export const testDir = path.dirname(fileURLToPath(import.meta.url));
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

export type Workspace = {
  root: string;
  /** A service directory inside the workspace, with no marker of its own. */
  service: string;
  /** A `.ts` file inside that service. */
  file: string;
  cleanup: () => void;
};

/**
 * A workspace laid out the way the local mode expects: `.carrick/` at the top,
 * service directories under it, and no marker inside a service.
 *
 * The counterpart files the fixtures name exist here too, because a counterpart
 * path is relative to its own repo and only resolves when that repo is on disk.
 */
export function makeWorkspace(): Workspace {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-plugin-"));
  fs.mkdirSync(path.join(root, ".carrick"), { recursive: true });
  const service = path.join(root, "user-service");
  fs.mkdirSync(path.join(service, "src", "routes"), { recursive: true });
  const file = path.join(service, "src", "routes", "users.ts");
  fs.writeFileSync(file, "export const handler = () => null;\n");
  for (const counterpart of [
    "order-service/src/clients/users.ts",
    "order-service/src/server.ts",
    "billing-service/src/lookup.ts",
    "billing-service/src/charges.ts",
  ]) {
    const target = path.join(root, counterpart);
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.writeFileSync(target, "export {};\n");
  }
  return {
    root,
    service,
    file,
    cleanup: () => fs.rmSync(root, { recursive: true, force: true }),
  };
}

export type HookRun = { stdout: string; stderr: string; code: number | null; ms: number };

/** Run one of the hook scripts the way Claude Code runs it: payload on stdin. */
export function runHook(
  script: string,
  options: { payload?: unknown; env?: NodeJS.ProcessEnv; cwd?: string },
): Promise<HookRun> {
  const started = Date.now();
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [path.join(pluginDir, "src", "hook", script)], {
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
    ...overrides,
  };
}
