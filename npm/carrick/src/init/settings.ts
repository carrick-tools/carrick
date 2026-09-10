// The hook entries `carrick init` writes into a project's `.claude/settings.json`.
//
// Merged entry by entry, never wholesale. Two things already write to that
// file: a user's own hooks, and the MCP hook pack the cloud's `scaffold` tool
// installs for the hosted index. Replacing the `hooks` key would silently
// uninstall either of them, so this only ever touches entries whose command is
// this CLI, and leaves the rest of the file — keys, order and formatting of
// what it does not own — as it found it.
//
// The command it writes is `carrick` when `carrick` resolves on PATH, and this
// install's own entry point when it does not: `npx carrick init` puts the shim
// on PATH for the length of that one command, so a bare `carrick` in a settings
// file it wrote would fail on every edit afterwards, silently, because the hook
// is built never to fail an edit (#837).

import path from "node:path";

import { packageRoot } from "../native.ts";

/**
 * The entry point in this package, for a settings file that must name a path.
 *
 * A hook command is a line handed to a shell, so this file has to be runnable
 * on its own: it carries a `#!/usr/bin/env node` shebang, it is executable in
 * the repository, and npm sets the mode on a `bin` target at install time
 * (verified 2026-09-09 against a packed tarball installed with
 * `--ignore-scripts`: 644 in the archive, 755 once installed).
 */
export function ownEntryPoint(root: string = packageRoot()): string {
  return path.join(root, "bin", "carrick.mjs");
}

export type HookCommandChoice = {
  /** What a hook entry runs, quoted if it has to be. */
  command: string;
  /** True when the bare name resolved and the settings file stays portable. */
  bare: boolean;
};

/**
 * The command to write into a settings file.
 *
 * `onPath` is injected so a test can state both machines; in `carrick init` it
 * is `which carrick`.
 */
export function hookCommand(options: {
  onPath: (command: string) => boolean;
  root?: string;
}): HookCommandChoice {
  if (options.onPath("carrick")) return { command: "carrick", bare: true };
  const entry = ownEntryPoint(options.root);
  return { command: /\s/.test(entry) ? `"${entry}"` : entry, bare: false };
}

export type HookEntry = { type: "command"; command: string; timeout?: number };
export type HookGroup = { matcher?: string; hooks: HookEntry[] };

/** What this package registers, and the only entries it will ever remove. */
export function carrickHooks(command = "carrick"): Record<string, HookGroup[]> {
  return {
    // Attached to the tool result, so the verdicts arrive in the same turn as
    // the edit rather than the turn after it.
    PostToolUse: [
      {
        matcher: "Write|Edit|MultiEdit",
        hooks: [{ type: "command", command: `${command} hook post-edit`, timeout: 15 }],
      },
    ],
    // No matcher: a resumed, cleared or compacted session has lost the map and
    // re-orients for one read of the index.
    SessionStart: [
      {
        hooks: [{ type: "command", command: `${command} hook session-start`, timeout: 30 }],
      },
    ],
  };
}

/**
 * Whether a settings entry is one of ours, whatever it names us by.
 *
 * An entry written on a machine with no `carrick` on PATH holds an absolute
 * path, so the test is the file it runs and the `hook` subcommand, not a fixed
 * prefix: otherwise re-running init on such a machine would leave the old entry
 * behind and add a second one.
 */
const HOOK_CALL = /^"?(.*?)"?\s+hook\s/;

function isOurs(entry: unknown): boolean {
  if (typeof entry !== "object" || entry === null) return false;
  const command = (entry as HookEntry).command;
  if (typeof command !== "string") return false;
  const match = HOOK_CALL.exec(command.trim());
  if (!match?.[1]) return false;
  const name = path.basename(match[1]);
  return name === "carrick" || name === "carrick.mjs";
}

/** Strip our entries from one event's groups, keeping everyone else's. */
function withoutOurs(groups: unknown): HookGroup[] {
  if (!Array.isArray(groups)) return [];
  const kept: HookGroup[] = [];
  for (const group of groups) {
    if (typeof group !== "object" || group === null) {
      kept.push(group as HookGroup);
      continue;
    }
    const hooks = (group as HookGroup).hooks;
    if (!Array.isArray(hooks)) {
      kept.push(group as HookGroup);
      continue;
    }
    const others = hooks.filter((entry) => !isOurs(entry));
    // A group that held only our entry goes with it; one that held a mix keeps
    // the rest of its entries and its matcher.
    if (others.length > 0) kept.push({ ...(group as HookGroup), hooks: others });
  }
  return kept;
}

export type MergeResult = {
  /** The file body to write. */
  body: string;
  /** True when it differs from what was there. */
  changed: boolean;
};

/**
 * Merge this package's hooks into a settings file, or produce one.
 *
 * Throws when the existing file is not JSON: a settings file a user has hand
 * edited into invalid JSON is a thing to report, never a thing to overwrite.
 */
export function mergeCarrickHooks(existing: string | null, command: string | null = "carrick"): MergeResult {
  const base: Record<string, unknown> =
    existing == null || existing.trim() === ""
      ? {}
      : (JSON.parse(existing) as Record<string, unknown>);

  const hooksBefore = (base["hooks"] ?? {}) as Record<string, unknown>;
  const hooks: Record<string, unknown> = {};
  for (const [event, groups] of Object.entries(hooksBefore)) {
    hooks[event] = withoutOurs(groups);
  }
  for (const [event, groups] of Object.entries(command === null ? {} : carrickHooks(command))) {
    hooks[event] = [...((hooks[event] as HookGroup[]) ?? []), ...groups];
  }
  // An event that only ever held our entry, and no longer does, leaves no
  // empty array behind to puzzle over.
  for (const [event, groups] of Object.entries(hooks)) {
    if (Array.isArray(groups) && groups.length === 0) delete hooks[event];
  }

  const merged: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(base)) {
    merged[key] = key === "hooks" ? hooks : value;
  }
  if (!("hooks" in merged)) merged["hooks"] = hooks;

  const body = `${JSON.stringify(merged, null, 2)}\n`;
  return { body, changed: body !== existing };
}
