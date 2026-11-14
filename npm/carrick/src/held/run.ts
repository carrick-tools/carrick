#!/usr/bin/env node
// The held-compiler daemon's entry: `node run.ts <checkout root>`.
//
// Started detached by `ensureHeld` (client.ts). With CARRICK_COMPILER_LINES
// unset it exits at once and touches nothing. It prints nothing and exits 0
// whatever happens.

import { execFile } from "node:child_process";
import { compilerLinesEnabled } from "../facts/enabled.ts";
import { binary } from "../cli.ts";
import { createLogger } from "../log.ts";
import { serve, type DaemonDeps, type ServiceInit } from "./daemon.ts";
import { createRuntime } from "./runtime.ts";

const log = createLogger("carrick-held-run");

const HELD_PROGRAMS_SCHEMA = "carrick.held-programs/0";

/** Every indexed service's program init, as the scanner loads it (`carrick held-programs --json`). */
function readPrograms(root: string, env: NodeJS.ProcessEnv): Promise<ServiceInit[]> {
  return new Promise((resolve, reject) => {
    execFile(binary(env), ["held-programs", "--json"], { cwd: root, env, maxBuffer: 16 * 1024 * 1024 }, (error, stdout) => {
      if (error) return reject(error);
      try {
        const parsed = JSON.parse(stdout) as { schema?: string; services?: ServiceInit[] };
        if (parsed.schema !== HELD_PROGRAMS_SCHEMA || !Array.isArray(parsed.services)) {
          return reject(new Error(`unexpected held-programs output (${parsed.schema ?? "no schema"})`));
        }
        resolve(parsed.services);
      } catch (parseError) {
        reject(parseError);
      }
    });
  });
}

async function main(): Promise<void> {
  const root = process.argv.slice(2)[0];
  if (!root) {
    log("no checkout root given");
    return;
  }
  const env = process.env;
  if (!compilerLinesEnabled(env)) return;
  const deps: DaemonDeps = { root, env, programs: () => readPrograms(root, env), runtime: createRuntime({ root, env }) };
  const stop = new AbortController();
  process.once("SIGTERM", () => stop.abort());
  process.once("SIGINT", () => stop.abort());
  const spawner = Number.parseInt(env["CARRICK_HELD_SPAWNER"] ?? "", 10);
  await serve(deps, { signal: stop.signal, ...(Number.isFinite(spawner) ? { spawner } : {}) });
}

try {
  await main();
} catch (error) {
  log("daemon failed", String(error));
}
process.exit(0);
