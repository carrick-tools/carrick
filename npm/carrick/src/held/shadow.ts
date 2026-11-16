// The daemon's one call into the compiler engine: `write_shadow` (D4, D5). The
// engine builds the service's program the way the scan does, writes the shadow
// tree a held-open compiler reads, and exits, so its memory is returned.

import fs from "node:fs";
import path from "node:path";
import { defaultExists } from "../fs-util.ts";
import { packageRoot, resolveSidecarDir } from "../native.ts";
import { askEngine } from "../engine-ask.ts";
import { heldDir } from "./paths.ts";
import type { SiteFinder, ValueReason } from "./place.ts";
import type { ServiceInit } from "./daemon.ts";

/** The three compiler options that change whether one type is assignable to another (the engine's `FitOptions`). */
export type FitOptions = { strictNullChecks: boolean; strictFunctionTypes: boolean; exactOptionalPropertyTypes: boolean };

/** What the daemon reads of `write_shadow`'s answer. */
export type ShadowAnswer = {
  /** Absolute path of the generated `w/tsconfig.json`. */
  tsconfig: string;
  /** The service's own files, relative to the scan root. */
  own_files: string[];
  /** The native TypeScript 7 executable, or null when only TypeScript 6 is installed (D12). */
  native_tsc: string | null;
  /** The judge's wire declarations and the one rule type `__carrick_Wire<S, E>` (S1). */
  wire: string[];
  /** The judge's other tolerances: the helper types a check composes (S1). The literal-widened one is an export suffix of a stub, and a laptop has no stub. */
  tolerances: { unwidened_alias_suffix: string; declarations: string[] };
  /** The options that decide whether one type fits another: as the shadow program was written, and as the judge checks. */
  options: { service: FitOptions; judge: FitOptions };
};

const SHADOW_TIMEOUT_MS = 10 * 60_000;
const SITE_TIMEOUT_MS = 30_000;

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
export async function writeShadow(entry: string, init: ServiceInit, out: string, env: NodeJS.ProcessEnv, home?: string): Promise<ShadowAnswer> {
  const cwd = heldDir(home);
  fs.mkdirSync(cwd, { recursive: true, mode: 0o700 });
  const frames = await askEngine(
    entry,
    [
      {
        action: "init",
        request_id: "init",
        repo_root: init.sidecar_root,
        scan_root: init.scan_root,
        ...(init.tsconfig === null ? {} : { tsconfig_path: init.tsconfig }),
      },
      { action: "write_shadow", request_id: "shadow", out_dir: out },
    ],
    { cwd, env, timeoutMs: SHADOW_TIMEOUT_MS },
  );
  const answer = frames.get("shadow");
  if (answer === undefined) throw new Error("no write_shadow answer");
  if (answer.status !== "success") throw new Error(`write_shadow failed: ${(answer.errors ?? []).join("; ")}`);
  return answer as unknown as ShadowAnswer;
}

/**
 * The compiler engine's `find_site`: where the node a stale span named is in
 * the new text (it reads no program and no disk, so one short process answers).
 * A reason is a success answer that carries `reason` (S1).
 */
export function engineSiteFinder(entry: string, env: NodeJS.ProcessEnv, home?: string): SiteFinder {
  return async (query) => {
    const cwd = heldDir(home);
    fs.mkdirSync(cwd, { recursive: true, mode: 0o700 });
    const frames = await askEngine(entry, [{ action: "find_site", request_id: "site", ...query }], { cwd, env, timeoutMs: SITE_TIMEOUT_MS });
    const answer = frames.get("site");
    if (answer === undefined || answer.status !== "success") throw new Error(`find_site failed: ${(answer?.errors ?? []).join("; ")}`);
    if (typeof answer["reason"] === "string") return { reason: answer["reason"] as ValueReason };
    return { start: Number(answer["start"]), end: Number(answer["end"]) };
  };
}
