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
// The folder `carrick init` itself runs in gets the same copy when it is the top
// of its own repository (`writeFolderCopy`), so a team's repo shows only the
// rules files init edits (carrick#4881). The two files git may track are the
// exceptions: a Codex hooks file the team tracks takes our entries as it always
// did, because Codex reads project hooks from nowhere else, and a settings file
// whose committed version already holds our entries is the team's and is left.
//
// Only a folder that is a git repository of its own gets a copy. A folder
// inside some other repository would put its lines in that repository's
// exclude file, anchored at that repository's root, where they name nothing.
//
// Reference: `docs/reference/task-skills.md`, "Where they are written".

import { realPath } from "../fs-util.ts";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

import { sameText, writeIfChanged } from "./files.ts";
import { LAUNCHER_FILE, launcherSource } from "./launcher.ts";
import { installedCarrickHooks, mergeCarrickHooks, mergeHookSet, removeCarrickHooks, SHARED_SETTINGS } from "./settings.ts";
import { CODEX_HOOKS_FILE, codexHooks, isEmptyHooksDocument } from "./codex.ts";
import {
  recordWrittenSkills,
  removeTaskSkills,
  renderTaskSkill,
  skillFile,
  skillState,
  taskSkillPaths,
  SKILL_ROOTS,
  skillRootLink,
  TASK_SKILLS,
  type SkillOutcome,
  type SkillScope,
} from "./task-skills.ts";

/** The line our block opens with, and the line it ends on. */
export const EXCLUDE_BEGIN = "# Added by carrick init: Carrick's hooks and skills for this clone";
export const EXCLUDE_END = "# End of carrick init lines";

/** The personal Claude Code settings file, which is the one a repo copy uses. */
export const LOCAL_SETTINGS = path.join(".claude", "settings.local.json");
const CLAUDE_LAUNCHER = path.join(".claude", LAUNCHER_FILE);
const CODEX_LAUNCHER = path.join(".codex", LAUNCHER_FILE);

/** Every file a repo copy can hold, relative to the repo. */
export function repoCopyPaths(): string[] {
  return [LOCAL_SETTINGS, CLAUDE_LAUNCHER, CODEX_HOOKS_FILE, CODEX_LAUNCHER, ...taskSkillPaths()];
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

/**
 * What `git` prints for these arguments in this folder, or null when it says
 * nothing (or runs past `timeoutMs`). `env`, when given, is git's whole
 * environment.
 */
export function git(repo: string, args: string[], timeoutMs = 5000, env?: NodeJS.ProcessEnv): string | null {
  const result = spawnSync("git", ["-C", repo, ...args], { encoding: "utf8", timeout: timeoutMs, maxBuffer: 4 * 1024 * 1024, ...(env ? { env } : {}) });
  if (result.status !== 0 || typeof result.stdout !== "string") return null;
  return result.stdout;
}

/** Whether this folder is the top of a git repository, not a folder inside one. */
export function ownRepository(repo: string): boolean {
  const top = git(repo, ["rev-parse", "--show-toplevel"])?.trim();
  return top !== undefined && top !== "" && realPath(top) === realPath(repo);
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
export function trackedPaths(repo: string, relatives: string[]): Set<string> | null {
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

/** What `git show HEAD:<file>` prints, or null where HEAD holds no such file (or no commit). */
function committedText(repo: string, relative: string): string | null {
  return git(repo, ["show", `HEAD:${relative.split(path.sep).join("/")}`]);
}

/** Whether a settings document holds an entry of ours; false for one that is not JSON. */
function holdsOurs(body: string | null): boolean {
  if (body === null) return false;
  try {
    return installedCarrickHooks(body).length > 0;
  } catch {
    return false;
  }
}

/** Whether the committed version of a settings file holds entries of ours. */
export function committedHooksAreOurs(repo: string, relative: string): boolean {
  return holdsOurs(committedText(repo, relative));
}

/** Two JSON documents that read the same, whatever their formatting. */
function sameDocument(left: string, right: string): boolean {
  try {
    return JSON.stringify(sortKeys(JSON.parse(left))) === JSON.stringify(sortKeys(JSON.parse(right)));
  } catch {
    return false;
  }
}

function sortKeys(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sortKeys);
  if (typeof value !== "object" || value === null) return value;
  return Object.fromEntries(
    Object.entries(value as Record<string, unknown>)
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
      .map(([key, entry]) => [key, sortKeys(entry)]),
  );
}

/** What writing a folder's copy did. */
export type FolderCopy = {
  /** The paths written, relative to the folder. */
  paths: string[];
  /** The Codex hooks file changed on this run: the first write is what asks the reader to trust it. */
  codexWritten: boolean;
  /** The team's committed settings file already holds our entries and was left as it is. */
  teamHooksLeft: boolean;
  /** Skill paths left alone, as `writeTaskSkills` reports them. */
  skills: SkillOutcome[];
  /** Hooks files that are not JSON: neither written nor recorded, the rest still is. */
  refused: Array<{ file: string; message: string }>;
};

type Planned = { relative: string; body: string; record: boolean };

/**
 * Plan and write one copy. `own` is the folder `carrick init` itself runs in,
 * which differs from a repo below it in three ways: a Codex hooks file git
 * tracks takes our entries, a settings file an older version edited is put
 * back, and a team's committed entries are left (see the header).
 */
function copyInto(repo: string, scope: SkillScope, own: boolean): FolderCopy | null {
  if (!ownRepository(repo)) return null;
  const exclude = excludeFile(repo);
  const tracked = exclude === null ? null : trackedPaths(repo, [...repoCopyPaths(), SHARED_SETTINGS]);
  if (exclude === null || tracked === null) {
    throw new Error("git could not say which files it tracks here");
  }
  const writes: Planned[] = [];
  const skills: SkillOutcome[] = [];
  const refused: FolderCopy["refused"] = [];
  // A hooks file that is not JSON is a file to report, never to overwrite. Init's
  // own folder reports it and still writes everything else, as the shared-file
  // path always did; a repo below a folder keeps its all-or-nothing copy.
  const hooksFile = (relative: string, body: () => string, record: boolean): void => {
    try {
      writes.push({ relative, body: body(), record });
    } catch (error) {
      if (!own) throw error;
      refused.push({ file: relative, message: (error as Error).message });
    }
  };
  // What init put into the shared settings file before it kept its own files out
  // of git, and the file it is put back to. Decided now, written last.
  let shared: { body: string } | { remove: true } | null = null;
  let teamHooksLeft = false;

  if (own) {
    const sharedFile = path.join(repo, SHARED_SETTINGS);
    const sharedBody = readOrNull(sharedFile);
    teamHooksLeft = committedHooksAreOurs(repo, SHARED_SETTINGS);
    if (!teamHooksLeft && sharedBody !== null && holdsOurs(sharedBody)) {
      const cleaned = removeCarrickHooks(sharedBody).body;
      const head = tracked.has(SHARED_SETTINGS) ? committedText(repo, SHARED_SETTINGS) : null;
      if (head !== null && sameDocument(cleaned, head)) shared = { body: head };
      else if (!tracked.has(SHARED_SETTINGS) && isEmptyHooksDocument(cleaned)) shared = { remove: true };
      else shared = { body: cleaned };
    }
  }

  const localExisting = readOrNull(path.join(repo, LOCAL_SETTINGS));
  if (!tracked.has(LOCAL_SETTINGS)) {
    if (teamHooksLeft) {
      // The committed entries run already; a second set here would run twice.
      if (holdsOurs(localExisting)) hooksFile(LOCAL_SETTINGS, () => removeCarrickHooks(localExisting!).body, true);
    } else {
      hooksFile(LOCAL_SETTINGS, () => mergeCarrickHooks(localExisting).body, true);
    }
    if (!(own && tracked.has(CLAUDE_LAUNCHER))) writes.push({ relative: CLAUDE_LAUNCHER, body: launcherSource(), record: true });
  }
  const codexExisting = readOrNull(path.join(repo, CODEX_HOOKS_FILE));
  if (!tracked.has(CODEX_HOOKS_FILE)) {
    hooksFile(CODEX_HOOKS_FILE, () => mergeHookSet(codexExisting, codexHooks()).body, true);
    writes.push({ relative: CODEX_LAUNCHER, body: launcherSource(), record: true });
  } else if (own) {
    // Codex reads project hooks from this one file, so there is no copy of it
    // to keep out of git; only the launcher its entries call stays hidden.
    hooksFile(CODEX_HOOKS_FILE, () => mergeHookSet(codexExisting, codexHooks()).body, false);
    writes.push({ relative: CODEX_LAUNCHER, body: launcherSource(), record: true });
  }
  for (const root of SKILL_ROOTS) {
    const link = skillRootLink(repo, root);
    if (link !== null) {
      for (const name of TASK_SKILLS) skills.push({ path: skillFile(root, name), state: "linked", wrote: false, link });
      continue;
    }
    for (const name of TASK_SKILLS) {
      const relative = skillFile(root, name);
      if (tracked.has(relative)) continue;
      // A skill of somebody else's at one of these paths is theirs to track or
      // not, so it is neither written nor excluded.
      const state = skillState(readOrNull(path.join(repo, relative)));
      if (state === "absent" || state === "ours") writes.push({ relative, body: renderTaskSkill(name, scope), record: true });
      else skills.push({ path: relative, state, wrote: false });
    }
  }
  // The record first: a write that fails after this leaves a file the record
  // names, which `carrick remove` can take back, never one it cannot see.
  writeIfChanged(exclude, withExcludeBlock(readOrNull(exclude), writes.filter((write) => write.record).map((write) => write.relative)));
  const changed = new Set<string>();
  for (const write of writes) {
    if (writeIfChanged(path.join(repo, write.relative), write.body) === "written") changed.add(write.relative);
  }
  // Init's own folder keeps the record self-update reads (`.carrick/task-skills-written.json`).
  if (own) recordWrittenSkills(repo, writes.map((write) => write.relative).filter((relative) => taskSkillPaths().includes(relative)));
  if (shared !== null) {
    const target = path.join(repo, SHARED_SETTINGS);
    if ("remove" in shared) fs.rmSync(target);
    else if (!sameText(readOrNull(target), shared.body)) fs.writeFileSync(target, shared.body);
  }
  return {
    paths: writes.filter((write) => write.record).map((write) => write.relative),
    codexWritten: changed.has(CODEX_HOOKS_FILE),
    teamHooksLeft,
    skills,
    refused,
  };
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
export function writeRepoCopy(repo: string, scope: SkillScope): string[] | null {
  return copyInto(repo, scope, false)?.paths ?? null;
}

/**
 * The same copy for the folder init runs in, when it is the top of its own
 * repository; null otherwise, and then init writes the shared files as it
 * always has.
 */
export function writeFolderCopy(folder: string, scope: SkillScope): FolderCopy | null {
  return copyInto(folder, scope, true);
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
 * The paths a repo copy's exclude block records, reading only.
 *
 * Null where the repo holds no block, which is every repo init never copied
 * into, and every folder that is not a repository of its own. The read half of
 * `removeRepoCopy`, so `carrick remove` can list a copy before it asks
 * (carrick#1573).
 */
export function recordedRepoCopy(repo: string): { exclude: string; block: ReturnType<typeof withoutExcludeBlock>; recorded: string[] } | null {
  if (!ownRepository(repo)) return null;
  const exclude = excludeFile(repo);
  const existing = exclude === null ? null : readOrNull(exclude);
  if (exclude === null || existing === null) return null;
  const block = withoutExcludeBlock(existing);
  if (!block.found) return null;
  return { exclude, block, recorded: block.lines.filter((line) => line.startsWith("/")).map(fromPattern) };
}

/**
 * Add paths to the repo's exclude block, which must exist already. The block is
 * written before any file it names, so a caller adds here first and writes after.
 * Returns false where the repo holds no block.
 */
export function addToExcludeBlock(repo: string, relatives: string[]): boolean {
  const copy = recordedRepoCopy(repo);
  if (copy === null) return false;
  const all = [...new Set([...copy.recorded, ...relatives])];
  writeIfChanged(copy.exclude, withExcludeBlock(readOrNull(copy.exclude), all));
  return true;
}

/**
 * Undo a repo copy: the paths our exclude block names, then the block.
 *
 * Null where the repo holds no block (see `recordedRepoCopy`). A folder
 * emptied on the way goes too, and so does an exclude file that held nothing
 * but the block.
 */
export function removeRepoCopy(repo: string): string[] | null {
  const copy = recordedRepoCopy(repo);
  if (copy === null) return null;
  const { exclude, block } = copy;
  const recorded = new Set(copy.recorded);
  for (const relative of [LOCAL_SETTINGS, CODEX_HOOKS_FILE]) {
    if (recorded.has(relative)) stripHooks(path.join(repo, relative));
  }
  for (const relative of [CLAUDE_LAUNCHER, CODEX_LAUNCHER]) {
    if (recorded.has(relative)) fs.rmSync(path.join(repo, relative), { force: true });
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
