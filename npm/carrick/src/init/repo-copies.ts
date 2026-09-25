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
// The exclude lines are the record. `carrick remove` reads them back to learn
// which files init wrote in the repo, undoes exactly those, and takes the lines
// out again.
//
// Reference: `docs/reference/task-skills.md`, "Where they are written".

import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

import { writeIfChanged } from "./files.ts";
import { mergeCarrickHooks, removeCarrickHooks } from "./settings.ts";
import { CODEX_HOOKS_FILE, isEmptyHooksDocument, uninstallCodexHooks, writeCodexHooks } from "./codex.ts";
import { removeTaskSkills, taskSkillPaths, writeTaskSkills, type SkillScope } from "./task-skills.ts";

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
 * A block whose end line has been deleted runs to the first line that is not
 * one of our paths, so a hand edit never lets this take a line of anyone
 * else's with it.
 */
export function withoutExcludeBlock(existing: string): { body: string; lines: string[]; found: boolean } {
  const all = existing.split("\n");
  const start = all.findIndex((line) => bare(line) === EXCLUDE_BEGIN);
  if (start === -1) return { body: existing, lines: [], found: false };
  const ours = new Set(repoCopyPaths().map(asPattern));
  let end = start + 1;
  const lines: string[] = [];
  while (end < all.length) {
    const line = bare(all[end]!);
    if (line === EXCLUDE_END) {
      end += 1;
      break;
    }
    if (!ours.has(line)) break;
    lines.push(line);
    end += 1;
  }
  return { body: [...all.slice(0, start), ...all.slice(end)].join("\n"), lines, found: true };
}

/**
 * The file with our block naming exactly these paths, and none where there are
 * none. A path the owner already excludes on a line of their own is named in
 * our block as well: the block is the record of what init wrote, and taking
 * the block out leaves the owner's line where it was.
 */
export function withExcludeBlock(existing: string | null, relatives: string[]): string {
  const { body } = withoutExcludeBlock(existing ?? "");
  const lines = relatives.map(asPattern);
  if (lines.length === 0) return body;
  const base = body === "" || body.endsWith("\n") ? body : `${body}\n`;
  return `${base}${EXCLUDE_BEGIN}\n${lines.join("\n")}\n${EXCLUDE_END}\n`;
}

/**
 * This clone's exclude file. Asked of git, because a linked worktree's `.git`
 * is a file and its exclude file is in the repository it was added from.
 */
export function excludeFile(repo: string): string | null {
  const result = spawnSync("git", ["-C", repo, "rev-parse", "--git-path", path.posix.join("info", "exclude")], {
    encoding: "utf8",
    timeout: 5000,
  });
  if (result.status !== 0 || typeof result.stdout !== "string") return null;
  const answer = result.stdout.trim();
  return answer === "" ? null : path.resolve(repo, answer);
}

/** The paths among these that git tracks here: the team's copies. */
function trackedPaths(repo: string, relatives: string[]): Set<string> | null {
  const result = spawnSync("git", ["-C", repo, "ls-files", "-z", "--", ...relatives.map((relative) => relative.split(path.sep).join("/"))], {
    encoding: "utf8",
    timeout: 5000,
  });
  if (result.status !== 0 || typeof result.stdout !== "string") return null;
  return new Set(result.stdout.split("\0").filter((entry) => entry !== "").map((entry) => entry.split("/").join(path.sep)));
}

/**
 * Write this repo's copy of the hooks and skills, and keep it out of git.
 *
 * Throws where git cannot say which files it tracks or where the exclude file
 * is: a copy that cannot be kept out of git would show up in every
 * `git status` in a repository somebody works in, so none is written.
 * Returns the paths written, relative to the repo.
 */
export function writeRepoCopy(repo: string, command: string, scope: SkillScope): string[] {
  const exclude = excludeFile(repo);
  const tracked = exclude === null ? null : trackedPaths(repo, repoCopyPaths());
  if (exclude === null || tracked === null) {
    throw new Error("git could not say which files it tracks here");
  }
  const written: string[] = [];
  if (!tracked.has(LOCAL_SETTINGS)) {
    const target = path.join(repo, LOCAL_SETTINGS);
    const existing = fs.existsSync(target) ? fs.readFileSync(target, "utf8") : null;
    writeIfChanged(target, mergeCarrickHooks(existing, command).body);
    written.push(LOCAL_SETTINGS);
  }
  if (!tracked.has(CODEX_HOOKS_FILE)) {
    writeCodexHooks(repo, command);
    written.push(CODEX_HOOKS_FILE);
  }
  for (const outcome of writeTaskSkills(repo, scope, (relative) => !tracked.has(relative))) {
    // A skill of somebody else's at one of these paths is theirs to track or
    // not, so it is neither written nor excluded.
    if (outcome.state === "absent" || outcome.state === "ours") written.push(outcome.path);
  }
  const before = fs.existsSync(exclude) ? fs.readFileSync(exclude, "utf8") : null;
  writeIfChanged(exclude, withExcludeBlock(before, written));
  return written;
}

/**
 * Undo a repo copy: the paths our exclude block names, then the block.
 *
 * Null where the repo holds no block, which is every repo init never copied
 * into. A settings file left holding nothing goes, because init is the reason
 * it exists; one holding anything of the owner's stays. A folder emptied on
 * the way goes too.
 */
export function removeRepoCopy(repo: string): string[] | null {
  const exclude = excludeFile(repo);
  if (exclude === null || !fs.existsSync(exclude)) return null;
  const existing = fs.readFileSync(exclude, "utf8");
  const block = withoutExcludeBlock(existing);
  if (!block.found) return null;
  const recorded = new Set(block.lines.map(fromPattern));
  if (recorded.has(LOCAL_SETTINGS)) {
    const target = path.join(repo, LOCAL_SETTINGS);
    if (fs.existsSync(target)) {
      const cleaned = removeCarrickHooks(fs.readFileSync(target, "utf8"));
      if (cleaned.changed) {
        if (isEmptyHooksDocument(cleaned.body)) fs.rmSync(target);
        else fs.writeFileSync(target, cleaned.body);
      }
    }
  }
  if (recorded.has(CODEX_HOOKS_FILE)) uninstallCodexHooks(repo);
  removeTaskSkills(repo, (relative) => recorded.has(relative));
  for (const directory of [".claude", ".codex", ".agents"]) {
    try {
      fs.rmdirSync(path.join(repo, directory));
    } catch {
      // Holds something else. That is the answer, not a failure.
    }
  }
  fs.writeFileSync(exclude, block.body);
  return [...recorded];
}
