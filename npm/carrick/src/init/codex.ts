// The hook entries `carrick init` writes into a project's `.codex/hooks.json`
// (carrick#1335).
//
// Codex reads hooks from `$CODEX_HOME/hooks.json` and from a project's
// `.codex/hooks.json` (`codex-rs/hooks/src/engine/discovery.rs`,
// `ConfigLayerSource::Project`). The project file is the one init writes: what
// it configures is this workspace, and a machine-wide file would follow the
// user to every repository they open.
//
// The document is the same shape as a `.claude` settings file — groups nested
// under a `hooks` key, `{ "type": "command", "command": ..., "timeout": ... }`
// entries, a `matcher` per group (`codex-rs/config/src/hook_config.rs`,
// `HooksFile`) — so the merge, the reader and the remover are the ones in
// `settings.ts` and only the entries differ. Same rule as the other file, for
// the same reason: entries whose command is this CLI are the only ones touched,
// and a hooks file a user already has keeps everything else in it.
//
// Two entries, which is the whole nudge on this host:
//
// * `PostToolUse` on `apply_patch`, which records what an edit added. The
//   matcher is a regex (`codex-rs/hooks/src/engine/dispatcher.rs`) and
//   `apply_patch` is the canonical tool name Codex puts on the payload.
// * `UserPromptSubmit`, which speaks the accumulated set once. Codex's `Stop`
//   event cannot emit `additionalContext` at all, so this is the only
//   non-blocking way to reach the model there — and the cost is that the line
//   arrives with the next prompt.
//
// One thing this file cannot do for the user: a project hook is untrusted until
// Codex records its hash, and an untrusted hook is discovered and not run
// (`hook_trust_status` in `discovery.rs`). Codex asks about it at its next
// start. `carrick init` says so; nothing here can answer it.
//
// Reference: `docs/reference/task-skills.md`, "The reuse nudge".

import fs from "node:fs";
import path from "node:path";

import { writeIfChanged } from "./files.ts";
import {
  flattenHooks,
  installedCarrickHooks,
  mergeHookSet,
  removeCarrickHooks,
  type HookGroup,
  type InstalledHook,
} from "./settings.ts";

/** Codex's project config folder, relative to the workspace. */
export const CODEX_DIR = ".codex";

/** The file init writes, relative to the workspace. */
export const CODEX_HOOKS_FILE = path.join(CODEX_DIR, "hooks.json");

/** What this package registers with Codex, and the only entries it removes. */
export function codexHooks(command = "carrick"): Record<string, HookGroup[]> {
  return {
    // One call can carry a patch touching several files, and each one is a
    // separate `carrick check`, so this timeout is not the single-file 15 the
    // Claude entry uses. Codex applies the value as written for this event
    // (`normalize_command_hook` clamps only SessionEnd and Interrupt).
    PostToolUse: [
      {
        matcher: "apply_patch",
        hooks: [{ type: "command", command: `${command} hook post-edit`, timeout: 60 }],
      },
    ],
    // The next prompt after the task that added the functions. It reads one
    // small file and prints nothing when there is nothing new, so the timeout
    // is the short one.
    UserPromptSubmit: [
      {
        hooks: [{ type: "command", command: `${command} hook user-prompt`, timeout: 5 }],
      },
    ],
  };
}

/** What `codexHooks` writes, flattened the way `installedCarrickHooks` reads. */
export function expectedCodexHooks(command = "carrick"): InstalledHook[] {
  return flattenHooks(codexHooks(command));
}

/**
 * Whether Codex is set up for this workspace.
 *
 * The test is its project config folder, which is the layer Codex reads project
 * hooks and project config from and the folder this file lives in. A workspace
 * with no `.codex/` has nothing of Codex's in it, and `carrick doctor` says
 * nothing about Codex there rather than warning every Claude Code user about a
 * host they do not run.
 */
export function codexInUse(workspace: string): boolean {
  try {
    return fs.statSync(path.join(workspace, CODEX_DIR)).isDirectory();
  } catch {
    return false;
  }
}

/** Our entries in a `.codex/hooks.json`, or an empty list where there is none. */
export function installedCodexHooks(workspace: string): InstalledHook[] {
  const body = readHooksFile(workspace);
  return body === null ? [] : installedCarrickHooks(body);
}

/** The file as it is, or null when it is not there. Throws nothing but I/O. */
export function readHooksFile(workspace: string): string | null {
  try {
    return fs.readFileSync(path.join(workspace, CODEX_HOOKS_FILE), "utf8");
  } catch {
    return null;
  }
}

export type CodexWrite = "written" | "unchanged";

/**
 * Write our entries into the project's Codex hooks file, merging.
 *
 * Throws on a file that is not JSON, exactly as the `.claude` writer does: a
 * file somebody hand-edited into something unparseable is reported, never
 * overwritten.
 */
export function writeCodexHooks(workspace: string, command: string): CodexWrite {
  const existing = readHooksFile(workspace);
  const merged = mergeHookSet(existing, codexHooks(command));
  return writeIfChanged(path.join(workspace, CODEX_HOOKS_FILE), merged.body);
}

/**
 * Whether what is left of a hooks file is nothing but the shape of one.
 *
 * `.codex/hooks.json` differs from a settings file in who it belongs to: init
 * is the reason it exists, so a file holding no hooks and nothing else after
 * our entries come out is ours to delete rather than an empty husk to leave in
 * somebody's repository. A `description`, a key we never write, or anyone
 * else's entry all make it theirs.
 */
export function isEmptyHooksDocument(body: string): boolean {
  let parsed: unknown;
  try {
    parsed = body.trim() === "" ? {} : JSON.parse(body);
  } catch {
    return false;
  }
  if (typeof parsed !== "object" || parsed === null) return false;
  const keys = Object.keys(parsed as Record<string, unknown>);
  if (keys.length > 1 || (keys.length === 1 && keys[0] !== "hooks")) return false;
  const hooks = (parsed as Record<string, unknown>)["hooks"];
  if (hooks === undefined) return true;
  return typeof hooks === "object" && hooks !== null && Object.keys(hooks).length === 0;
}

export type CodexRemoval = "removed" | "file removed" | null;

/**
 * Take our entries out again, and the file with them where it held only ours.
 *
 * Null when there was nothing of ours there, so `carrick remove` stays silent
 * about a machine that never installed it. A `.codex` directory emptied by the
 * deletion goes too; one holding anything else of Codex's stays.
 */
export function uninstallCodexHooks(workspace: string): CodexRemoval {
  const existing = readHooksFile(workspace);
  if (existing === null) return null;
  const cleaned = removeCarrickHooks(existing);
  if (!cleaned.changed) return null;
  const target = path.join(workspace, CODEX_HOOKS_FILE);
  if (!isEmptyHooksDocument(cleaned.body)) {
    fs.writeFileSync(target, cleaned.body);
    return "removed";
  }
  fs.rmSync(target);
  try {
    fs.rmdirSync(path.join(workspace, CODEX_DIR));
  } catch {
    // Holds something else of Codex's. That is the answer, not a failure.
  }
  return "file removed";
}
