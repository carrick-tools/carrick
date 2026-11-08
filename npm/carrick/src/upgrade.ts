// The upgrade a detached child performs, and the record it leaves for the next
// session (docs/reference/update-check.md, "Upgrade an existing global").
//
// `src/self-upgrade.ts` is the entry point; the logic lives here so a test can
// drive it with a fake machine. The child only ever upgrades a global install
// that already exists, through `syncGlobalInstall`, to a release newer than the
// one running and inside the same major version. It prints nothing and fails
// open: any error ends it with no record.
//
// Static imports only, and no dynamic `import()` after the install starts: the
// package manager replaces this package's own files while the child runs.

import fs from "node:fs";
import path from "node:path";

import { configDir } from "./auth/credentials.ts";
import { realMachine, syncGlobalInstall } from "./global-install.ts";
import type { GlobalMachine } from "./global-install.ts";
import { writeAtomic } from "./hook/reuse.ts";
import { currentVersion, fetchLatest, inCi, isNewer, suppressed } from "./update.ts";

/** What an attempt ended in. Attempts that changed nothing leave no record. */
export type UpgradeRecord = {
  kind: "upgraded" | "refused" | "stale";
  from: string | null;
  to: string;
  warning: string | null;
  at: string;
  /** Set once a session has told the reader. */
  reported?: boolean;
};

export function outcomePath(env: NodeJS.ProcessEnv = process.env): string {
  return path.join(configDir(env), "self-upgrade.json");
}

export function readOutcome(env: NodeJS.ProcessEnv = process.env): UpgradeRecord | null {
  try {
    const parsed = JSON.parse(fs.readFileSync(outcomePath(env), "utf8"));
    const kind = parsed?.kind;
    if (kind !== "upgraded" && kind !== "refused" && kind !== "stale") return null;
    if (typeof parsed.to !== "string" || typeof parsed.at !== "string") return null;
    return {
      kind,
      from: typeof parsed.from === "string" ? parsed.from : null,
      to: parsed.to,
      warning: typeof parsed.warning === "string" ? parsed.warning : null,
      at: parsed.at,
      ...(parsed.reported === true ? { reported: true } : {}),
    };
  } catch {
    return null;
  }
}

export function writeOutcome(record: UpgradeRecord, env: NodeJS.ProcessEnv = process.env): boolean {
  return writeAtomic(outcomePath(env), `${JSON.stringify(record, null, 2)}\n`);
}

/** Whether two plain versions share a major number: no upgrade crosses one. */
export function sameMajor(a: string, b: string): boolean {
  const major = (value: string): string | null => /^(\d+)\.\d+\.\d+$/.exec(value.trim())?.[1] ?? null;
  const left = major(a);
  return left !== null && left === major(b);
}

export type UpgradeOptions = {
  env?: NodeJS.ProcessEnv;
  machine?: GlobalMachine;
  /** The version running; this package's own when absent. */
  current?: string | null;
  /** A fresh registry answer, never the cache: a stale cache once downgraded an install. */
  latest?: () => Promise<string | null>;
};

/** Returns the record it wrote, or null when it changed nothing or failed. */
export async function runUpgrade(options: UpgradeOptions = {}): Promise<UpgradeRecord | null> {
  try {
    const env = options.env ?? process.env;
    if (suppressed(env) || inCi(env)) return null;
    const current = options.current === undefined ? currentVersion() : options.current;
    if (!current) return null;
    const fresh = await (options.latest ?? (() => fetchLatest()))();
    if (!fresh || !isNewer(fresh, current) || !sameMajor(fresh, current)) return null;
    const outcome = syncGlobalInstall({
      target: fresh,
      say: () => {},
      env,
      ...(options.machine ? { machine: options.machine } : {}),
    });
    if (outcome.kind !== "upgraded" && outcome.kind !== "refused" && outcome.kind !== "stale") return null;
    const record: UpgradeRecord = {
      kind: outcome.kind,
      from: outcome.kind === "stale" ? (outcome.version ?? current) : outcome.from,
      to: fresh,
      warning: outcome.warning,
      at: new Date().toISOString(),
      reported: false,
    };
    writeOutcome(record, env);
    return record;
  } catch {
    return null;
  }
}
