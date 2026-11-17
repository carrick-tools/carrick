// The launcher every agent-hook entry calls (carrick#4759).
//
// A hook entry that runs a bare `carrick` fails with `command not found` once
// the default Node changes, because a global install lives under the Node that
// installed it, and no Carrick code runs to say why. So every entry calls
// `carrick-hook.sh`, a small POSIX sh file committed beside the settings file.
// The file's one source is `templates/carrick-hook.sh`; this module writes it,
// recognises it and asks it where it would run Carrick from. The lookup lives
// in that file only: `carrick doctor` runs it with `--where` rather than
// keeping a second copy of the search.
//
// Windows: the launcher is sh, which Claude Code on Windows runs through Git
// Bash; Codex on Windows runs hooks through PowerShell and is not covered.

import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

import { sameText, writeIfChanged } from "./files.ts";
import { templatesDir } from "../templates.ts";

/** The launcher's file name, in the folder of the file that calls it. */
export const LAUNCHER_FILE = "carrick-hook.sh";

/**
 * What a Claude Code entry runs before `hook <event>`. `$CLAUDE_PROJECT_DIR`
 * is set by Claude Code for every hook, so no machine path is in the file.
 */
export const CLAUDE_LAUNCHER_CALL = `sh "$CLAUDE_PROJECT_DIR/.claude/${LAUNCHER_FILE}"`;

/**
 * The same for Codex, which sets no project variable: the repository root,
 * or the working folder where there is no git.
 */
export const CODEX_LAUNCHER_CALL = `sh "$(git rev-parse --show-toplevel 2>/dev/null || pwd)/.codex/${LAUNCHER_FILE}"`;

/** The plugin's entries: the plugin root holds its own copy of the launcher. */
export const PLUGIN_LAUNCHER_CALL = `sh "\${CLAUDE_PLUGIN_ROOT}/hooks/${LAUNCHER_FILE}"`;

/** Where the shipped launcher sits in this package. */
export function launcherPath(): string {
  return path.join(templatesDir(), LAUNCHER_FILE);
}

/** The launcher's text, the one source every copy is written from. */
export function launcherSource(): string {
  return fs.readFileSync(launcherPath(), "utf8");
}

/** Write the launcher into a folder (`.claude` or `.codex`), where it would change. */
export function writeLauncher(folder: string): "written" | "unchanged" {
  return writeIfChanged(path.join(folder, LAUNCHER_FILE), launcherSource());
}

/** Whether the launcher in a folder is missing or not this version's. */
export function launcherDiffers(folder: string): boolean {
  try {
    return !sameText(fs.readFileSync(path.join(folder, LAUNCHER_FILE), "utf8"), launcherSource());
  } catch {
    return true;
  }
}

/** Take the launcher out of a folder. True when there was one. */
export function removeLauncher(folder: string): boolean {
  const target = path.join(folder, LAUNCHER_FILE);
  if (!fs.existsSync(target)) return false;
  fs.rmSync(target);
  return true;
}

/**
 * The carrick the launcher would run from this shell, or null.
 *
 * Asks the launcher itself (`--where`), so the answer is the one a hook gets.
 */
export function launcherWhere(env: NodeJS.ProcessEnv = process.env): string | null {
  const probe = spawnSync("sh", [launcherPath(), "--where"], {
    encoding: "utf8",
    timeout: 5000,
    env,
  });
  if (probe.status !== 0 || typeof probe.stdout !== "string") return null;
  return probe.stdout.split("\n")[0]?.trim() || null;
}

/**
 * The Node an install's path names, for the layouts that put it there
 * (`/versions/node/v24.9.0/`, `/node-versions/v24.9.0/`, `/installs/nodejs/24.9.0/`).
 */
export function installNode(binary: string): string | null {
  const match = /\/(?:versions\/node|node-versions|installs\/nodejs|installs\/node)\/v?(\d+(?:\.\d+){0,2})\//.exec(
    binary.split(path.sep).join("/"),
  );
  return match?.[1] ?? null;
}
