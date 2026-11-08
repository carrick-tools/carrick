// Session start brings Carrick's own installed files up to date.
//
// `npm install -g carrick@latest` changes the binary and nothing else, and the
// session-start line used to end in "Run `carrick init`" for exactly that
// difference. A person should not have to say that to an agent every upgrade, so
// the hook that already knows both numbers does the rewrite itself.
//
// What it touches, and nothing more:
//
// * Our own hook entries in `.claude/settings.json` and `.claude/settings.local.json`,
//   merged in place with `mergeCarrickHooks` so a user's own hooks and every other
//   key stay. Never moved between the two files (`init/run.ts` does that; this does
//   not). A file holding no entry of ours, or not JSON, is left as it is.
// * The task skills that are still ours and untouched (`inspectTaskSkills`: state
//   `ours`, not current), under both harness roots. They carry a digest stamp and
//   no Codex trust check (that is on hook files only, `codex.ts`), so the
//   `.agents/skills` copies move with the `.claude/skills` ones. An edited skill,
//   somebody else's, one that was never written, and a root that is a link out of
//   its harness folder are left alone.
//
// * The Carrick rules section in the folder's `AGENTS.md`, between its two marker
//   comments and nowhere else (`agents-section.ts`), rewritten to what this CLI
//   renders for the scope and counts in `.carrick/hosted/snapshot.json`. Never
//   added where the markers are absent, and left alone when the snapshot is
//   missing or does not name one project (a section that named a project is not
//   downgraded).
//
// Both harnesses run it (carrick#3435). A Codex session has no Claude Code
// project dir, so it passes `projectDir: null` and the settings files above wait
// for a Claude Code session; the skills and the AGENTS.md section, which Codex
// reads with no trust gate, move the same way under either.
//
// `.codex/hooks.json` is never written: a changed project hook is untrusted until
// Codex records its hash (`codex.ts`), so only `carrick init` plus the user's
// approval can move it.
//
// Reference: `docs/reference/task-skills.md`.

import { snapshotRepos } from "../hosted-snapshot.ts";
import fs from "node:fs";
import path from "node:path";

import { snapshotProjectSlug } from "../hook/reuse.ts";
import { isNewer } from "../update.ts";
import { AGENTS_FILE, CLAUDE_FILE, renderCheckSection, withCheckSection, type WorkspaceCounts } from "./agents-section.ts";
import {
  entriesAreStale,
  outdatedFiles,
  outdatedInstall,
  recordCliVersion,
  recordedCliVersion,
} from "./outdated.ts";
import { expectedCarrickHooks, installedCarrickHooks, mergeCarrickHooks, SETTINGS_FILES } from "./settings.ts";
import { inspectTaskSkills, renderTaskSkill, scopeOf, TASK_SKILLS, type TaskSkill } from "./task-skills.ts";

/** Which settings files were rewritten for which version, so one removed on purpose stays removed. */
export function updatedStampFile(workspace: string): string {
  return path.join(workspace, ".carrick", "hook-entries-updated.json");
}

function readStamp(workspace: string): Record<string, string> {
  try {
    const parsed: unknown = JSON.parse(fs.readFileSync(updatedStampFile(workspace), "utf8"));
    return typeof parsed === "object" && parsed !== null ? (parsed as Record<string, string>) : {};
  } catch {
    return {};
  }
}

/** Written beside the file and renamed over it, so a reader never sees half a document. */
function writeAtomic(target: string, body: string): void {
  const temp = `${target}.${process.pid}.tmp`;
  try {
    fs.writeFileSync(temp, body);
    fs.renameSync(temp, target);
  } catch (error) {
    fs.rmSync(temp, { force: true });
    throw error;
  }
}

/**
 * Our hook entries that an older version wrote, rewritten in place. Returns the
 * files written: a file the merge leaves as it is is neither stamped nor reported.
 * `merge` is a parameter so a test can hold a file the merge does not move.
 */
export function updateHooks(
  workspace: string,
  projectDir: string,
  running: string,
  merge: typeof mergeCarrickHooks = mergeCarrickHooks,
): string[] {
  const stamp = readStamp(workspace);
  const changed: string[] = [];
  for (const relative of SETTINGS_FILES) {
    const target = path.join(projectDir, relative);
    // Keyed by the file's path under the workspace, so a repo's copy is its own entry.
    const key = path.relative(workspace, target);
    if (stamp[key] === running) continue;
    try {
      const body = fs.readFileSync(target, "utf8");
      if (!entriesAreStale(body, expectedCarrickHooks)) continue;
      const prefix = installedCarrickHooks(body)[0]!.command.split(/\s+hook\s+/)[0]!;
      const merged = merge(body, prefix);
      if (!merged.changed) continue;
      writeAtomic(target, merged.body);
      stamp[key] = running;
      changed.push(relative);
    } catch {
      // Unreadable or not JSON: a file to report (`carrick doctor` does), never to overwrite.
    }
  }
  if (changed.length > 0) {
    try {
      writeAtomic(updatedStampFile(workspace), `${JSON.stringify(stamp, null, 2)}\n`);
    } catch {
      // The entries are current; the stamp only guards a later deliberate removal.
    }
  }
  return changed;
}

/** Task skills still ours and an older body, rewritten with the scope each was written with. */
function updateSkills(workspace: string): string[] {
  const changed: string[] = [];
  for (const skill of inspectTaskSkills(workspace)) {
    if (skill.state !== "ours" || skill.current) continue;
    const name = path.basename(path.dirname(skill.path)) as TaskSkill;
    if (!TASK_SKILLS.includes(name)) continue;
    try {
      const target = path.join(workspace, skill.path);
      writeAtomic(target, renderTaskSkill(name, scopeOf(fs.readFileSync(target, "utf8"))));
      changed.push(skill.path);
    } catch {
      // Left as it was; the next session tries again.
    }
  }
  return changed;
}

/** Repos and services the snapshot's resolution holds; null where it cannot be read. */
function snapshotCounts(workspace: string): WorkspaceCounts | null {
  const repos = snapshotRepos(workspace);
  if (repos === null || repos.length === 0) return null;
  let services = 0;
  for (const repo of repos) {
    const held = (repo as { services?: unknown } | null)?.services;
    if (Array.isArray(held)) services += held.length;
  }
  return { repos: repos.length, services };
}

/**
 * Our AGENTS.md section, rewritten when it differs from what this CLI renders for
 * the snapshot. Returns the file written, or null. `log` says why a section was
 * left alone.
 */
function updateAgentsSection(workspace: string, log: (message: string) => void): string | null {
  const target = path.join(workspace, AGENTS_FILE);
  try {
    const existing = fs.readFileSync(target, "utf8");
    if (!existing.includes("<!-- carrick:check start -->")) return null;
    const slug = snapshotProjectSlug(workspace);
    const counts = snapshotCounts(workspace);
    if (slug === "" || counts === null) {
      log("AGENTS.md section left as it is: no readable snapshot naming one project");
      return null;
    }
    const body = withCheckSection(existing, renderCheckSection({ slug }, counts));
    if (body === existing) return null;
    writeAtomic(target, body);
    return AGENTS_FILE;
  } catch {
    return null;
  }
}

/**
 * The same block in a CLAUDE.md the workspace has (carrick#3515): refreshed, or
 * added where the file exists without it. Only in a workspace whose AGENTS.md
 * holds our marker, so a folder init never wrote to is left alone. Returns the
 * file written, or null.
 */
function updateClaudeSection(workspace: string): string | null {
  const target = path.join(workspace, CLAUDE_FILE);
  try {
    const agents = fs.readFileSync(path.join(workspace, AGENTS_FILE), "utf8");
    if (!agents.includes("<!-- carrick:check start -->")) return null;
    const existing = fs.readFileSync(target, "utf8");
    const slug = snapshotProjectSlug(workspace);
    const counts = snapshotCounts(workspace);
    if (slug === "" || counts === null) return null;
    const body = withCheckSection(existing, renderCheckSection({ slug }, counts));
    if (body === existing) return null;
    writeAtomic(target, body);
    return CLAUDE_FILE;
  } catch {
    return null;
  }
}

/**
 * Bring this workspace's Carrick files up to date for the running version.
 *
 * Returns the lines to print: one each for the hook entries, the skills and the AGENTS.md section, none
 * where nothing needed doing. Skipped whole when the running version is older
 * than the one recorded in `.carrick/cli-version` (the mismatch line tells the
 * person to replace that install, and a rewrite would downgrade the files).
 */
export function bringUpToDate(
  workspace: string,
  projectDir: string | null,
  running: string | null,
  log: (message: string) => void = () => {},
): string[] {
  if (!running || !/^\d+\.\d+\.\d+$/.test(running)) return [];
  const recorded = recordedCliVersion(workspace);
  if (recorded !== null && isNewer(recorded, running)) return [];
  const lines: string[] = [];
  const hooks = projectDir === null ? [] : updateHooks(workspace, projectDir, running);
  if (hooks.length > 0) {
    lines.push(`Carrick brought its hook entries in ${hooks.join(", ")} up to date for ${running}.`);
  }
  const skills = updateSkills(workspace);
  if (skills.length > 0) {
    const roots = [...new Set(skills.map((file) => path.dirname(path.dirname(file))))];
    lines.push(`Carrick brought its task skills in ${roots.join(", ")} up to date for ${running}.`);
  }
  const written = [updateAgentsSection(workspace, log), updateClaudeSection(workspace)].filter(
    (file): file is string => file !== null,
  );
  if (written.length > 0) {
    lines.push(`Carrick brought its rules section in ${written.join(" and ")} up to date for ${running}.`);
  }
  // The recorded version is what the mismatch line compares. Move it only when
  // nothing of ours is still old: a Codex hook file we may not touch keeps the
  // "run carrick init" line honest.
  if (recorded !== null && recorded !== running && outdatedFiles(outdatedInstall(workspace)).length === 0) {
    recordCliVersion(workspace, running);
  }
  return lines;
}

