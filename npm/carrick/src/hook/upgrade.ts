// The session-start half of the CLI upgrading itself (docs/reference/update-check.md).
//
// Replaces the plain version notice. Three outcomes, none of which waits on the
// network or a package manager:
//
//  - the last attempt's result, said once;
//  - an attempt started, for a global install that is behind the cached
//    `latest` and has not been tried at that version;
//  - today's notice, for everything else: a project dependency, an npx run, a
//    machine that switched the check off, CI, a major version jump, a release
//    already refused, a claim another session holds.
//
// Fails open: an error anywhere prints nothing and starts nothing.

import path from "node:path";
import { fileURLToPath } from "node:url";

import { configDir } from "../auth/credentials.ts";
import { globalCommand } from "../global-install.ts";
import { readOutcome, sameMajor, writeOutcome } from "../upgrade.ts";
import {
  CHECK_TTL_MS,
  currentVersion,
  inCi,
  installShape,
  isNewer,
  readUpdateState,
  suppressed,
  updateNotice,
} from "../update.ts";
import { claimRefresh, startDetached } from "./refresh.ts";

export type UpgradeLinesOptions = {
  env?: NodeJS.ProcessEnv;
  now?: number;
  /** The package root the running code was loaded from; decides the install shape. */
  root?: string;
  /** The running version; this package's own when absent. */
  current?: string | null;
  /** Injected in tests; the real one detaches a child and forgets it. */
  start?: (command: string, args: string[], env: NodeJS.ProcessEnv) => void;
};

export function upgradeLines(options: UpgradeLinesOptions = {}): string[] {
  try {
    const env = options.env ?? process.env;
    if (suppressed(env)) return [];
    const now = options.now ?? Date.now();
    const current = options.current === undefined ? currentVersion() : options.current;
    const lines: string[] = [];

    const record = readOutcome(env);
    let warned = false;
    if (record && !record.reported) {
      const said =
        record.kind === "upgraded"
          ? `Carrick updated itself from ${record.from ?? "an earlier version"} to ${record.to}.`
          : record.warning;
      writeOutcome({ ...record, reported: true }, env);
      if (said) {
        lines.push(said);
        warned = record.kind !== "upgraded";
      }
    }

    const latest = readUpdateState(env)?.latest ?? null;
    if (!current || !latest || !isNewer(latest, current)) return lines;
    const notice = updateNotice(current, latest, {
      env,
      ...(options.root ? { root: options.root } : {}),
    });

    const started = tryStart({ env, now, current, latest, root: options.root, start: options.start });
    if (started) lines.push(started);
    else if (notice && !(warned && record?.to === latest)) lines.push(notice);
    return lines;
  } catch {
    return [];
  }
}

function tryStart(input: {
  env: NodeJS.ProcessEnv;
  now: number;
  current: string;
  latest: string;
  root: string | undefined;
  start: UpgradeLinesOptions["start"];
}): string | null {
  const { env, now, current, latest } = input;
  if (inCi(env) || !sameMajor(latest, current)) return null;
  const shape = input.root ? installShape(input.root, env) : installShape(undefined, env);
  const argv = globalCommand(shape.kind, latest);
  if (!argv) return null;
  const last = readOutcome(env);
  if (last && last.to === latest && (last.kind === "refused" || last.kind === "stale")) return null;
  const marker = path.join(configDir(env), `self-upgrade-${latest}.claim`);
  if (!claimRefresh(marker, now, CHECK_TTL_MS)) return null;
  try {
    (input.start ?? startDetached)(
      process.execPath,
      [fileURLToPath(new URL("../self-upgrade.js", import.meta.url))],
      env,
    );
  } catch {
    return null;
  }
  return `carrick ${current} is installed and ${latest} is published. Updating it in the background with \`${argv.join(" ")}\`; the next session runs ${latest}.`;
}
