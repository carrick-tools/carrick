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
    // The end of a task, which is the only moment a reuse check is worth a
    // model turn: the post-edit hook records what each edit added and this
    // names the lot once (carrick#1330). It reads one small file and prints
    // nothing when the task added no function, so the timeout is the short one.
    Stop: [
      {
        hooks: [{ type: "command", command: `${command} hook stop`, timeout: 5 }],
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

/** One hook entry of ours, as a settings file holds it. */
export type InstalledHook = {
  /** `PostToolUse`, `SessionStart`. */
  event: string;
  /** The group's matcher, absent where the group has none. */
  matcher?: string;
  command: string;
  timeout?: number;
};

/**
 * Our entries in a settings document, in the order the file holds them.
 *
 * The read half of the pair, for a command that audits an install rather than
 * changing one (`carrick doctor`, carrick#1035). It exists because the merge
 * cannot answer this question: `mergeCarrickHooks(...).changed` is false for a
 * file that already holds the current entries AND for one that holds an entry
 * this version no longer writes, and comparing a rewritten document to the
 * original says nothing about which entries are there (carrick#1034).
 *
 * Throws on a document that is not JSON, like the merge, so a hand-edited file
 * is reported rather than read as empty.
 */
export function installedCarrickHooks(existing: string): InstalledHook[] {
  const base: unknown = existing.trim() === "" ? {} : JSON.parse(existing);
  if (typeof base !== "object" || base === null) return [];
  const hooks = (base as Record<string, unknown>)["hooks"];
  if (typeof hooks !== "object" || hooks === null) return [];
  const found: InstalledHook[] = [];
  for (const [event, groups] of Object.entries(hooks as Record<string, unknown>)) {
    if (!Array.isArray(groups)) continue;
    for (const group of groups) {
      const entries = (group as HookGroup | null)?.hooks;
      if (!Array.isArray(entries)) continue;
      const matcher = (group as HookGroup).matcher;
      for (const entry of entries) {
        if (!isOurs(entry)) continue;
        const installed: InstalledHook = { event, command: (entry as HookEntry).command };
        if (typeof matcher === "string") installed.matcher = matcher;
        if (typeof (entry as HookEntry).timeout === "number") {
          installed.timeout = (entry as HookEntry).timeout;
        }
        found.push(installed);
      }
    }
  }
  return found;
}

/** What `carrickHooks` writes, flattened the way `installedCarrickHooks` reads. */
export function expectedCarrickHooks(command = "carrick"): InstalledHook[] {
  const expected: InstalledHook[] = [];
  for (const [event, groups] of Object.entries(carrickHooks(command))) {
    for (const group of groups) {
      for (const entry of group.hooks) {
        const one: InstalledHook = { event, command: entry.command };
        if (group.matcher !== undefined) one.matcher = group.matcher;
        if (entry.timeout !== undefined) one.timeout = entry.timeout;
        expected.push(one);
      }
    }
  }
  return expected;
}

/**
 * The command an installed entry runs, without the `hook <name>` after it.
 *
 * `"/opt/my tools/carrick/bin/carrick.mjs" hook post-edit` gives the path with
 * its quotes stripped, and a bare `carrick hook post-edit` gives `carrick`, so
 * a caller can ask whether that command still resolves on this machine.
 */
export function hookTarget(command: string): string | null {
  const match = HOOK_CALL.exec(command.trim());
  return match?.[1] ?? null;
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

/** Whether a settings document holds an entry of ours at all. */
function holdsCarrickHooks(base: unknown): boolean {
  if (typeof base !== "object" || base === null) return false;
  const hooks = (base as Record<string, unknown>)["hooks"];
  if (typeof hooks !== "object" || hooks === null) return false;
  for (const groups of Object.values(hooks as Record<string, unknown>)) {
    if (!Array.isArray(groups)) continue;
    for (const group of groups) {
      const entries = (group as HookGroup | null)?.hooks;
      if (Array.isArray(entries) && entries.some(isOurs)) return true;
    }
  }
  return false;
}

/**
 * The inverse of `mergeCarrickHooks`: our entries out, everything else kept.
 *
 * `carrick remove` runs this over both settings files a workspace can hold
 * (carrick#1034). It is the merge with nothing to add: whatever `isOurs`
 * recognises is what init wrote, and nothing else in the file moves. The file
 * itself stays — a settings file holds a user's own hooks and permissions, and
 * init was never the reason it exists.
 *
 * A file holding no entry of ours is returned byte for byte, and reported as
 * unchanged. The merge would otherwise reformat it and add an empty `hooks`
 * key, which is a rewrite of somebody else's file and a `◇ removed` line about
 * nothing — and that file is often committed.
 */
export function removeCarrickHooks(existing: string): MergeResult {
  const base: unknown = existing.trim() === "" ? {} : JSON.parse(existing);
  if (!holdsCarrickHooks(base)) return { body: existing, changed: false };
  return mergeCarrickHooks(existing, null);
}
