// Carrick's hooks and skills inside each repo of a folder `carrick init` set up
// (carrick#1512).
//
// Run in the folder that holds the repos, init writes its hooks and skills
// there, and an agent started inside one of the repos never reads them:
// Claude Code reads `.claude/` from the directory it starts in and walks up
// only as far as the repository root for skills, and Codex resolves its hooks
// and `.agents/skills` from the git root the same way. So each repo gets a
// copy of its own, and the copy stays out of git through the repo's
// `.git/info/exclude`, which belongs to this clone and is never committed.
// The copy a team commits is the scaffold pull request's business.
//
// The Claude Code hooks go in `.claude/settings.local.json`, the personal
// file, never in `settings.json`, which the scaffold commits. A path git
// already tracks is left alone: it is the team's copy.
//
// The exclude lines are the record, and they are written before any file they
// name, so a copy that fails halfway never leaves a hook nobody recorded.
// `carrick remove` reads them back to learn which files init wrote in the
// repo, undoes exactly those, and takes the lines out again.
//
// Only a folder that is a git repository of its own gets a copy. A folder
// inside some other repository would put its lines in that repository's
// exclude file, anchored at that repository's root, where they name nothing.
//
// Reference: `docs/reference/task-skills.md`, "Where they are written".

import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

import { writeIfChanged } from "./files.ts";
import { mergeCarrickHooks, mergeHookSet, removeCarrickHooks } from "./settings.ts";
import { CODEX_HOOKS_FILE, codexHooks, isEmptyHooksDocument } from "./codex.ts";
import {
  removeTaskSkills,
  renderTaskSkill,
  skillFile,
  skillState,
  taskSkillPaths,
  SKILL_ROOTS,
  TASK_SKILLS,
  type SkillScope,
} from "./task-skills.ts";

/** The line our block opens with, and the line it ends on. */
export const EXCLUDE_BEGIN = "# Added by carrick init: Carrick's hooks and skills for this clone";
export const EXCLUDE_END = "# End of carrick init lines";

/** The personal Claude Code settings file, which is the one a repo copy uses. */
export const LOCAL_SETTINGS = path.join(".claude", "settings.local.json");

/** Every file a repo copy can hold, relative to the repo. */
export function repoCopyPaths(): string[] {
  return [LOCAL_SETTINGS, CODEX_HOOKS_FILE, ...taskSkillPaths()];
}

/** A relative path as an exclude line: anchored to the repo root, `/`-separated. */
function asPattern(relative: string): string {
  return `/${relative.split(path.sep).join("/")}`;
}

/** An exclude line of ours back to the path it names. */
function fromPattern(line: string): string {
  return line.replace(/^\//, "").split("/").join(path.sep);
}

/** One line with its line ending taken off. */
function bare(line: string): string {
  return line.replace(/\r$/, "");
}

/**
 * The file without our block, and the lines the block held.
 *
 * The block is every line from the opening marker to the closing one. One
 * whose closing line somebody deleted runs to the first line that is not one
 * of our paths, so a hand edit never takes a line of anyone else's with it.
 * Taking the block out of the lines and joining the rest gives back the file
 * as it was, byte for byte, final newline or not.
 */
export function withoutExcludeBlock(existing: string): { body: string; lines: string[]; found: boolean } {
  const all = existing.split("\n");
  const start = all.findIndex((line) => bare(line) === EXCLUDE_BEGIN);
  if (start === -1) return { body: existing, lines: [], found: false };
  const close = all.findIndex((line, index) => index > start && bare(line) === EXCLUDE_END);
  let end: number;
  if (close !== -1) end = close + 1;
  else {
    const ours = new Set(repoCopyPaths().map(asPattern));
    end = start + 1;
    while (end < all.length && ours.has(bare(all[end]!))) end += 1;
  }
  return {
    body: [...all.slice(0, start), ...all.slice(end)].join("\n"),
    lines: all.slice(start + 1, close !== -1 ? close : end).map(bare),
    found: true,
  };
}

/**
 * The file with our block naming exactly these paths, and none where there are
 * none. The block goes first, so the file after it is the owner's, untouched,
 * whatever it ends in. A path the owner already excludes on a line of their
 * own is named in our block as well: the block is the record of what init
 * wrote, and taking it out leaves the owner's line where it was.
 */
export function withExcludeBlock(existing: string | null, relatives: string[]): string {
  const { body } = withoutExcludeBlock(existing ?? "");
  if (relatives.length === 0) return body;
  return `${EXCLUDE_BEGIN}\n${relatives.map(asPattern).join("\n")}\n${EXCLUDE_END}\n${body}`;
}

/** What `git` prints for these arguments in this folder, or null when it says nothing. */
function git(repo: string, args: string[]): string | null {
  const result = spawnSync("git", ["-C", repo, ...args], { encoding: "utf8", timeout: 5000 });
  if (result.status !== 0 || typeof result.stdout !== "string") return null;
  return result.stdout;
}

/** A path with its symlinks resolved, or as given where it cannot be. */
function real(target: string): string {
  try {
    return fs.realpathSync(target);
  } catch {
    return path.resolve(target);
  }
}

/** Whether this folder is the top of a git repository, not a folder inside one. */
export function ownRepository(repo: string): boolean {
  const top = git(repo, ["rev-parse", "--show-toplevel"])?.trim();
  return top !== undefined && top !== "" && real(top) === real(repo);
}

/**
 * This clone's exclude file. Asked of git, because a linked worktree's `.git`
 * is a file and its exclude file is in the repository it was added from.
 */
export function excludeFile(repo: string): string | null {
  const answer = git(repo, ["rev-parse", "--git-path", path.posix.join("info", "exclude")])?.trim();
  return answer === undefined || answer === "" ? null : path.resolve(repo, answer);
}

/** The paths among these that git tracks here: the team's copies. */
function trackedPaths(repo: string, relatives: string[]): Set<string> | null {
  const answer = git(repo, ["ls-files", "-z", "--", ...relatives.map((relative) => relative.split(path.sep).join("/"))]);
  if (answer === null) return null;
  return new Set(answer.split("\0").filter((entry) => entry !== "").map((entry) => entry.split("/").join(path.sep)));
}

function readOrNull(target: string): string | null {
  try {
    return fs.readFileSync(target, "utf8");
  } catch {
    return null;
  }
}

/**
 * Write this repo's copy of the hooks and skills, and keep it out of git.
 *
 * Null where the folder is not a git repository of its own. Throws where git
 * cannot say which files it tracks or where the exclude file is, and where a
 * file to merge into is not JSON; in all of these nothing is written, because
 * every body is decided before the first write. Returns the paths written,
 * relative to the repo.
 */
export function writeRepoCopy(repo: string, command: string, scope: SkillScope): string[] | null {
  if (!ownRepository(repo)) return null;
  const exclude = excludeFile(repo);
  const tracked = exclude === null ? null : trackedPaths(repo, repoCopyPaths());
  if (exclude === null || tracked === null) {
    throw new Error("git could not say which files it tracks here");
  }
  const writes: Array<{ relative: string; body: string }> = [];
  if (!tracked.has(LOCAL_SETTINGS)) {
    const existing = readOrNull(path.join(repo, LOCAL_SETTINGS));
    writes.push({ relative: LOCAL_SETTINGS, body: mergeCarrickHooks(existing, command).body });
  }
  if (!tracked.has(CODEX_HOOKS_FILE)) {
    const existing = readOrNull(path.join(repo, CODEX_HOOKS_FILE));
    writes.push({ relative: CODEX_HOOKS_FILE, body: mergeHookSet(existing, codexHooks(command)).body });
  }
  for (const root of SKILL_ROOTS) {
    for (const name of TASK_SKILLS) {
      const relative = skillFile(root, name);
      if (tracked.has(relative)) continue;
      // A skill of somebody else's at one of these paths is theirs to track or
      // not, so it is neither written nor excluded.
      const state = skillState(readOrNull(path.join(repo, relative)));
      if (state === "absent" || state === "ours") writes.push({ relative, body: renderTaskSkill(name, scope) });
    }
  }
  // The record first: a write that fails after this leaves a file the record
  // names, which `carrick remove` can take back, never one it cannot see.
  writeIfChanged(exclude, withExcludeBlock(readOrNull(exclude), writes.map((write) => write.relative)));
  for (const write of writes) writeIfChanged(path.join(repo, write.relative), write.body);
  return writes.map((write) => write.relative);
}

/**
 * Our hook entries out of one file, and the file with them where nothing else
 * is left in it: init is the reason it exists. A file of hooks somebody else
 * also wrote into keeps theirs.
 */
function stripHooks(target: string): void {
  const existing = readOrNull(target);
  if (existing === null) return;
  const cleaned = removeCarrickHooks(existing);
  const left = cleaned.changed ? cleaned.body : existing;
  if (isEmptyHooksDocument(left)) fs.rmSync(target);
  else if (cleaned.changed) fs.writeFileSync(target, left);
}

/**
 * Undo a repo copy: the paths our exclude block names, then the block.
 *
 * Null where the repo holds no block, which is every repo init never copied
 * into, and every folder that is not a repository of its own. A folder emptied
 * on the way goes too, and so does an exclude file that held nothing but the
 * block.
 */
export function removeRepoCopy(repo: string): string[] | null {
  if (!ownRepository(repo)) return null;
  const exclude = excludeFile(repo);
  const existing = exclude === null ? null : readOrNull(exclude);
  if (exclude === null || existing === null) return null;
  const block = withoutExcludeBlock(existing);
  if (!block.found) return null;
  const recorded = new Set(block.lines.filter((line) => line.startsWith("/")).map(fromPattern));
  for (const relative of [LOCAL_SETTINGS, CODEX_HOOKS_FILE]) {
    if (recorded.has(relative)) stripHooks(path.join(repo, relative));
  }
  removeTaskSkills(repo, (relative) => recorded.has(relative));
  for (const directory of [".claude", ".codex", ".agents"]) {
    try {
      fs.rmdirSync(path.join(repo, directory));
    } catch {
      // Holds something else. That is the answer, not a failure.
    }
  }
  if (block.body === "") fs.rmSync(exclude);
  else fs.writeFileSync(exclude, block.body);
  return [...recorded];
}
