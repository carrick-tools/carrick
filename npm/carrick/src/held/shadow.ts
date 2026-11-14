// The daemon's one call into the compiler engine: `write_shadow` (D4, D5). The
// engine builds the service's program the way the scan does, writes the shadow
// tree a held-open compiler reads, and exits, so its memory is returned.

import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { defaultExists } from "../fs-util.ts";
import { packageRoot, resolveSidecarDir } from "../native.ts";
import { heldDir } from "./paths.ts";
import type { ServiceInit } from "./daemon.ts";

/** What the daemon reads of `write_shadow`'s answer. */
export type ShadowAnswer = {
  /** Absolute path of the generated `w/tsconfig.json`. */
  tsconfig: string;
  /** The service's own files, relative to the scan root. */
  own_files: string[];
  /** Some own file imports a package named `hono`. */
  hono: boolean;
  /** The native TypeScript 7 executable, or null when only TypeScript 6 is installed (D12). */
  native_tsc: string | null;
};

const SHADOW_TIMEOUT_MS = 10 * 60_000;

/** The built compiler engine's entry, or null when none is built. */
export function engineEntry(env: NodeJS.ProcessEnv = process.env): string | null {
  const dirs = [resolveSidecarDir({ env }), path.resolve(packageRoot(), "..", "..", "src", "sidecar")];
  for (const dir of dirs) {
    if (dir === null) continue;
    const entry = path.join(dir, "dist", "src", "index.js");
    if (defaultExists(entry)) return entry;
  }
  return null;
}

/**
 * Run the compiler engine once: `init` as the scan sends it for this service,
 * then `write_shadow` into `out`. The process runs from `~/.carrick/held/`,
 * outside every checkout, because the loader resolves `types` references from
 * its working directory.
 */
export function writeShadow(entry: string, init: ServiceInit, out: string, env: NodeJS.ProcessEnv, home?: string): Promise<ShadowAnswer> {
  const cwd = heldDir(home);
  fs.mkdirSync(cwd, { recursive: true, mode: 0o700 });
  const requests = [
    {
      action: "init",
      request_id: "init",
      repo_root: init.sidecar_root,
      scan_root: init.scan_root,
      ...(init.tsconfig === null ? {} : { tsconfig_path: init.tsconfig }),
    },
    { action: "write_shadow", request_id: "shadow", out_dir: out },
    { action: "shutdown", request_id: "stop" },
  ];
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [entry], { cwd, env, stdio: ["pipe", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    const timer = setTimeout(() => child.kill("SIGKILL"), SHADOW_TIMEOUT_MS);
    child.stdout.on("data", (chunk: Buffer) => (stdout += chunk.toString("utf8")));
    child.stderr.on("data", (chunk: Buffer) => (stderr = (stderr + chunk.toString("utf8")).slice(-2000)));
    child.on("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.on("close", () => {
      clearTimeout(timer);
      for (const line of stdout.split("\n")) {
        if (!line.trim()) continue;
        try {
          const answer = JSON.parse(line) as { request_id?: string; status?: string; errors?: string[] } & Partial<ShadowAnswer>;
          if (answer.request_id !== "shadow") continue;
          if (answer.status !== "success") return reject(new Error(`write_shadow failed: ${(answer.errors ?? []).join("; ")}`));
          return resolve(answer as ShadowAnswer);
        } catch {
          // Not a protocol line.
        }
      }
      reject(new Error(`no write_shadow answer: ${stderr}`));
    });
    child.stdin.write(requests.map((r) => JSON.stringify(r)).join("\n") + "\n");
    child.stdin.end();
  });
}
