// The four task skills `carrick init` installs beside the hooks.
//
// A skill is the body a hook or an agent's own judgement points at, and these
// four perform a task rather than remind: each one is a sequence of Carrick
// tool calls, and the agent's work is to confirm what came back in source,
// classify it, decide and act.
//
// `docs/reference/task-skills.md` is what each one covers, when it fires and
// how the stamp below decides who owns a file. The bodies themselves are the
// markdown in `templates/skills/`, so they can be read and reviewed as files.
//
// Two copies of the same bytes go in, one per harness: `.claude/skills/` for
// Claude Code and `.agents/skills/` for Codex, which is the scaffold parity
// rule of 2026-09-05.
//
// Every file this writes ends in a stamp: a marker and a digest of the body
// above it. The digest is what separates the three states a path can be in on
// a re-run — ours and untouched, ours and since edited, or somebody else's —
// and it stays right across an upgrade, where the shipped body changes and a
// version number would not. `carrick remove` deletes the untouched ones and
// nothing else, so a file a user has made their own is never destroyed by a
// command undoing an install.
//
// The reminder skill at `.claude/skills/carrick/SKILL.md` belongs to the cloud
// scaffold and carries no stamp, so nothing here reads or writes it.

import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

import { templatesDir } from "../templates.ts";
import { writeIfChanged } from "./files.ts";

/** The skills, in the order `carrick init` reports them. */
export const TASK_SKILLS = [
  "carrick-impact",
  "carrick-reuse",
  "carrick-drift",
  "carrick-census",
] as const;

export type TaskSkill = (typeof TASK_SKILLS)[number];

/** One directory per harness. Both hold identical bytes. */
export const SKILL_ROOTS = [
  path.join(".claude", "skills"),
  path.join(".agents", "skills"),
];

/** Where one skill lands under one root, relative to the workspace. */
export function skillFile(root: string, name: TaskSkill): string {
  return path.join(root, name, "SKILL.md");
}

/** Every path these skills occupy, relative to the workspace. */
export function taskSkillPaths(): string[] {
  return SKILL_ROOTS.flatMap((root) => TASK_SKILLS.map((name) => skillFile(root, name)));
}

const STAMP = /^<!-- carrick:skill sha256:([0-9a-f]{12}) -->$/;

/** The digest of a body, as the stamp spells it. */
function digest(body: string): string {
  return createHash("sha256").update(body, "utf8").digest("hex").slice(0, 12);
}

/** A rendered body with its stamp on the last line. */
export function stamped(body: string): string {
  const text = body.endsWith("\n") ? body : `${body}\n`;
  return `${text}\n<!-- carrick:skill sha256:${digest(text)} -->\n`;
}

/**
 * What a path holds now.
 *
 * `ours` is a file this package wrote and nobody has touched since, whichever
 * version wrote it. `edited` carries our stamp over a body that has moved, and
 * `theirs` carries no stamp at all: a skill of the same name somebody wrote
 * themselves. Only `absent` and `ours` are written over.
 */
export type SkillState = "absent" | "ours" | "edited" | "theirs";

export function skillState(existing: string | null): SkillState {
  if (existing === null) return "absent";
  // Line endings are normalised before anything is compared. A checkout with
  // `core.autocrlf` set rewrites every one of these files to CRLF on the way
  // to disk, and a digest taken over those bytes would read all eight as
  // somebody else's work and warn about them on every run.
  const lines = existing.replace(/\r\n/g, "\n").split("\n");
  // The marker is looked for anywhere rather than on the last line, because
  // appending a step under it is as ordinary an edit as changing one above it,
  // and a file that still carries our marker is still one of ours.
  const marker = lines.findIndex((line) => STAMP.test(line));
  if (marker === -1) return "theirs";
  // The blank line `stamped` puts before the marker is the join's last
  // separator, so rejoining everything above it reproduces the exact bytes
  // that were digested.
  const body = lines.slice(0, marker).join("\n");
  const after = lines.slice(marker + 1).join("").trim();
  const claimed = STAMP.exec(lines[marker]!)?.[1];
  return digest(body) === claimed && after === "" ? "ours" : "edited";
}

/** How a call names the project on every Carrick tool call in a body. */
export type SkillScope = { slug: string | null };

function scopeArgument(scope: SkillScope): string {
  return scope.slug === null ? 'repo: "<owner/repo>"' : `project: "${scope.slug}"`;
}

function scopeNote(scope: SkillScope): string {
  return scope.slug === null
    ? "Every Carrick call below needs a scope. Read this repository's once with `git remote get-url origin` and pass it as `repo: \"<owner/repo>\"` wherever the commands below write it."
    : `Every Carrick call below carries \`project: "${scope.slug}"\`, which is this workspace's Carrick project and is already written into the commands.`;
}

/** One skill body, scoped and stamped, ready to write. */
export function renderTaskSkill(name: TaskSkill, scope: SkillScope): string {
  const source = fs.readFileSync(path.join(templatesDir(), "skills", `${name}.md`), "utf8");
  const rendered = source
    .split("{{SCOPE_NOTE}}")
    .join(scopeNote(scope))
    .split("{{SCOPE}}")
    .join(scopeArgument(scope));
  // A body that still holds a placeholder is a file that looks written and
  // tells an agent to call a tool with a literal `{{SCOPE}}` in it.
  const left = /\{\{([A-Z_]+)\}\}/.exec(rendered);
  if (left) throw new Error(`${name} still holds ${left[0]} after rendering`);
  return stamped(rendered);
}

/**
 * The scope a file on disk was written with, read back out of it.
 *
 * The digest below is taken over a rendered body, and a body renders
 * differently for a workspace with a project and one without — so telling an
 * older version's file from the current one means rendering the same scope it
 * was written with. The rendered scope is in the file, as the only `project:
 * "…"` a body can hold (the templates carry `{{SCOPE}}` and no literal), and a
 * body written without one says `repo: "<owner/repo>"` instead.
 */
const RENDERED_SCOPE = /project: "([^"]+)"/;

export function scopeOf(body: string): SkillScope {
  const named = RENDERED_SCOPE.exec(body);
  return { slug: named ? named[1]! : null };
}

/** The digest a file's own stamp claims, or null where it carries none. */
function claimedDigest(existing: string): string | null {
  const marker = existing
    .replace(/\r\n/g, "\n")
    .split("\n")
    .find((line) => STAMP.test(line));
  return marker ? (STAMP.exec(marker)?.[1] ?? null) : null;
}

/** What one installed path holds, for a reader that changes nothing. */
export type InstalledSkill = {
  /** Relative to the workspace. */
  path: string;
  state: SkillState;
  /**
   * True where the bytes are the ones this package renders now.
   *
   * Only meaningful for a file in state `ours`: an upgrade that does not
   * change a body leaves that body current, which is why this is a comparison
   * of content and not of version numbers. A skill somebody edited is not
   * measured against the current render at all — it is theirs now, and saying
   * it is also out of date would be two findings about one file.
   */
  current: boolean;
};

/**
 * What is installed under both roots right now.
 *
 * The read half of `writeTaskSkills`, for `carrick doctor` and for the line
 * any command can print (carrick#1331, carrick#1333). It opens the eight
 * files, renders each one against the scope its own body carries, and says
 * nothing about a workspace — the caller decides which of these four states is
 * worth a finding.
 */
export function inspectTaskSkills(workspace: string): InstalledSkill[] {
  const installed: InstalledSkill[] = [];
  for (const root of SKILL_ROOTS) {
    for (const name of TASK_SKILLS) {
      const relative = skillFile(root, name);
      let existing: string | null;
      try {
        existing = fs.readFileSync(path.join(workspace, relative), "utf8");
      } catch {
        existing = null;
      }
      const state = skillState(existing);
      const current =
        state === "ours" && existing !== null
          ? claimedDigest(existing) === claimedDigest(renderTaskSkill(name, scopeOf(existing)))
          : false;
      installed.push({ path: relative, state, current });
    }
  }
  return installed;
}

/** What one path did on this run. */
export type SkillOutcome = {
  /** Relative to the workspace. */
  path: string;
  state: SkillState;
  /** True where this run put new bytes there. */
  wrote: boolean;
};

/**
 * Write every skill under both roots, leaving alone anything that is not ours.
 *
 * Idempotent twice over: a second run renders the same bytes, and
 * `writeIfChanged` does not touch a file that already holds them.
 *
 * `only` narrows the paths, relative to the workspace: a repo's own copy skips
 * the ones its git tracks, which are the team's (carrick#1512).
 */
export function writeTaskSkills(
  workspace: string,
  scope: SkillScope,
  only: (relative: string) => boolean = () => true,
): SkillOutcome[] {
  const outcomes: SkillOutcome[] = [];
  for (const root of SKILL_ROOTS) {
    for (const name of TASK_SKILLS) {
      const relative = skillFile(root, name);
      if (!only(relative)) continue;
      const target = path.join(workspace, relative);
      const existing = fs.existsSync(target) ? fs.readFileSync(target, "utf8") : null;
      const state = skillState(existing);
      if (state === "edited" || state === "theirs") {
        outcomes.push({ path: relative, state, wrote: false });
        continue;
      }
      const result = writeIfChanged(target, renderTaskSkill(name, scope));
      outcomes.push({ path: relative, state, wrote: result === "written" });
    }
  }
  return outcomes;
}

/** The lines `carrick init` prints about what it just did with them. */
export function taskSkillLines(outcomes: SkillOutcome[]): { done: string[]; warn: string[] } {
  const done: string[] = [];
  const warn: string[] = [];
  const installed = outcomes.filter((row) => row.state === "absent" || row.state === "ours");
  if (installed.length > 0) {
    done.push(
      `Task skills installed in ${SKILL_ROOTS.join(" and ")}: ${TASK_SKILLS.join(", ")}`,
    );
  }
  for (const row of outcomes) {
    if (row.state === "edited") {
      warn.push(
        `${row.path} has been edited here, so it was left as it is. Delete it and run carrick init again for the current version.`,
      );
    } else if (row.state === "theirs") {
      warn.push(`${row.path} was not written by Carrick, so it was left as it is.`);
    }
  }
  return { done, warn };
}

/**
 * Delete the skill files this package wrote, and only those.
 *
 * An edited body and a file of somebody else's are returned as `kept` for the
 * caller to name, exactly as the write path leaves them. A skill directory
 * emptied by the deletion goes with it; a root holding anything else stays.
 *
 * `only` narrows the paths the way it does for the write: a repo's own copy
 * is undone for exactly the paths it was written to (carrick#1512).
 */
export function removeTaskSkills(
  workspace: string,
  only: (relative: string) => boolean = () => true,
): { deleted: string[]; kept: SkillOutcome[] } {
  const deleted: string[] = [];
  const kept: SkillOutcome[] = [];
  for (const root of SKILL_ROOTS) {
    for (const name of TASK_SKILLS) {
      const relative = skillFile(root, name);
      if (!only(relative)) continue;
      const target = path.join(workspace, relative);
      if (!fs.existsSync(target)) continue;
      const state = skillState(fs.readFileSync(target, "utf8"));
      if (state !== "ours") {
        kept.push({ path: relative, state, wrote: false });
        continue;
      }
      fs.rmSync(target);
      deleted.push(relative);
      // The skill's own directory, the root that held it, and the host folder
      // that root was the only thing in: `.agents/` exists because init wrote
      // skills into it, and an empty one left behind is litter a user would
      // have to recognise before deleting (carrick#1331). `.claude/` holds
      // their settings and their own files, so the same call does nothing
      // there — a directory that is not empty is not removed.
      for (const directory of [
        path.dirname(target),
        path.join(workspace, root),
        path.join(workspace, root.split(path.sep)[0]!),
      ]) {
        try {
          fs.rmdirSync(directory);
        } catch {
          // Holds something else. That is the answer, not a failure.
        }
      }
    }
  }
  return { deleted, kept };
}

/**
 * The skill roots this repository ignores.
 *
 * An ignored skill works on the machine that ran `carrick init` and reaches
 * nobody else on the team, which is a thing to say once rather than a thing to
 * change: what a repository ignores is its own decision.
 *
 * `git check-ignore` answers for a path that does not exist yet. Exit 0 is
 * ignored and 1 is not; anything else (128 outside a repository) is git
 * declining to answer, and this says nothing then.
 */
export function ignoredSkillRoots(workspace: string): string[] {
  return SKILL_ROOTS.filter((root) => {
    const probe = spawnSync("git", ["check-ignore", "-q", "--", root], {
      cwd: workspace,
      stdio: "ignore",
    });
    return probe.status === 0;
  });
}
