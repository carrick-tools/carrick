// The Carrick rules section `carrick init` keeps in a folder's AGENTS.md
// (carrick#2828), so an agent in Codex or Claude Code runs the local check
// before it says a change is done.
//
// The section sits between two marker comments. Those markers are the whole
// ownership rule: a re-run replaces what is between them and nothing else,
// `carrick remove` deletes that span and nothing else, and every other line of
// the file is somebody's and is never read for meaning. The file is created
// when it is missing, and deleted again on remove only when our section was
// all it held.
//
// The same block goes into a CLAUDE.md the workspace already has (carrick#3515):
// a Claude Code session started in a subfolder does not follow the `@AGENTS.md`
// import, so the block is written inline there too. CLAUDE.md is never created,
// and never deleted, because init did not create it.

import fs from "node:fs";
import path from "node:path";

import { writeIfChanged } from "./files.ts";
import { scopeArgument, type SkillScope } from "./task-skills.ts";

export const AGENTS_FILE = "AGENTS.md";
export const CLAUDE_FILE = "CLAUDE.md";

const START = "<!-- carrick:check start -->";
const END = "<!-- carrick:check end -->";

/** What init knows of the workspace when it writes the section. */
export type WorkspaceCounts = { repos: number; services: number };

/** "three repositories, 43 services" as the section says it; null where init has no count to give. */
function countsPhrase(counts: WorkspaceCounts | null): string | null {
  if (counts === null || counts.repos < 1 || counts.services < 1) return null;
  const repos = counts.repos === 1 ? "one repository" : `${counts.repos} repositories`;
  const services = counts.services === 1 ? "1 service" : `${counts.services} services`;
  return `${repos}, ${services}`;
}

/**
 * The section as written, markers included. Ends in one newline.
 *
 * The words are the rules block measured in carrick-cloud#2991 (21 graded runs
 * per arm on one build: reused 17/21 against 5/21), plus rule 5's contract
 * sentence: without it agents ran `check --changed` on 0 of 7 tasks that add a
 * call to another service, against 6 of 7 on the previous section. Rule 5 asks
 * for that check only where another service reads the change and leaves the
 * per-file check to the edit hook init installs for Claude Code and Codex: the
 * same 4 of 7 contract checks as the per-file wording, with no duplicate
 * per-file calls (carrick-cloud#3068, D13). Only the counts and the scope clause
 * vary; edit nothing else without a new measurement.
 */
export function renderCheckSection(scope: SkillScope, counts: WorkspaceCounts | null): string {
  const phrase = countsPhrase(counts);
  const indexed = phrase === null ? "This workspace is indexed by Carrick." : `This workspace (${phrase}) is indexed by Carrick.`;
  return [
    START,
    "## Carrick: use it while you work, not only at the end",
    "",
    `${indexed} The`,
    "`mcp__carrick__*` tools answer questions about code outside the file you are",
    "editing more completely than grep: across every service and every protocol",
    "(HTTP routes, socket events, pub/sub topics), including callers reached through",
    "SDKs, wrappers, client instances and untyped helpers that share no searchable",
    `name with what you changed. Pass \`${scopeArgument(scope)}\` on every Carrick`,
    "call.",
    "",
    "1. Before you write a new function, helper or utility, call `search_by_intent`",
    "   (or `find_similar`) with a plain-English description of what it does. If",
    "   something in any package already does the job, or most of it, import it and",
    "   call it instead of writing your own. When the task names a new function or",
    "   file, still create it, but make its body call the existing function and add",
    "   only what the existing one lacks. Never leave a second copy of logic that",
    "   already exists.",
    "2. Before you change a route, a request or response shape, an event payload or",
    "   an exported function, call `get_operation` (method and path, or the event or",
    "   topic name) or `get_callers`, and update or explicitly clear every consumer",
    "   it lists, in every repository.",
    "3. Before you write code that calls another service's endpoint or handles its",
    "   event, call `get_endpoint_types` (or `get_contract_pair`) and use the real",
    "   request and response types it returns. Never take a shape from the ticket, a",
    "   similar endpoint or another API version, or a local copy of a type.",
    "4. Use grep and Read to confirm and to read code once Carrick has told you",
    "   where to look.",
    "5. Run `carrick check --changed` before you say it is done only when your change",
    "   touches a route, a call to another service, an event or a type another",
    "   service reads. Each file you edit is already checked for you, so a change no",
    "   other service sees needs nothing more. In your final summary, say what",
    "   Carrick found and what you changed because of it.",
    END,
    "",
  ].join("\n");
}

/** The span of our section in a body, end included; null where there is none. */
function span(body: string): { from: number; to: number } | null {
  const from = body.indexOf(START);
  if (from === -1) return null;
  const end = body.indexOf(END, from);
  if (end === -1) return null;
  const after = end + END.length;
  return { from, to: body[after] === "\n" ? after + 1 : after };
}

/** The body with our section written in: replaced where it is, appended where it is not. */
export function withCheckSection(existing: string | null, section: string): string {
  if (existing === null || existing.trim() === "") return section;
  const found = span(existing);
  if (found !== null) return existing.slice(0, found.from) + section + existing.slice(found.to);
  const lf = existing.endsWith("\n") ? existing : `${existing}\n`;
  return `${lf}\n${section}`;
}

/** The body with our section taken out; `empty` where nothing else was in it. */
export function withoutCheckSection(existing: string): { body: string; changed: boolean; empty: boolean } {
  const found = span(existing);
  if (found === null) return { body: existing, changed: false, empty: false };
  // The blank line `withCheckSection` put before the section goes with it.
  let from = found.from;
  const before = existing.slice(0, from);
  if (/\n\n$/.test(before)) from -= 1;
  const body = existing.slice(0, from) + existing.slice(found.to);
  return { body, changed: true, empty: body.trim() === "" };
}

/** Write our section into the folder's AGENTS.md. */
export function writeAgentsSection(
  workspace: string,
  scope: SkillScope,
  counts: WorkspaceCounts | null,
): "written" | "unchanged" {
  const target = path.join(workspace, AGENTS_FILE);
  const existing = fs.existsSync(target) ? fs.readFileSync(target, "utf8") : null;
  return writeIfChanged(target, withCheckSection(existing, renderCheckSection(scope, counts)));
}

/** True where the folder's AGENTS.md holds our section. */
export function hasAgentsSection(workspace: string): boolean {
  const target = path.join(workspace, AGENTS_FILE);
  return fs.existsSync(target) && span(fs.readFileSync(target, "utf8")) !== null;
}

/** Take our section out. The file goes too only where nothing else was in it. */
export function removeAgentsSection(workspace: string): "section removed" | "file removed" | null {
  const target = path.join(workspace, AGENTS_FILE);
  if (!fs.existsSync(target)) return null;
  const cleaned = withoutCheckSection(fs.readFileSync(target, "utf8"));
  if (!cleaned.changed) return null;
  if (cleaned.empty) {
    fs.rmSync(target);
    return "file removed";
  }
  fs.writeFileSync(target, cleaned.body);
  return "section removed";
}

/**
 * Write our section into the workspace's CLAUDE.md, only where that file exists.
 * Returns null where there is none: init never creates it (carrick#3515).
 */
export function writeClaudeSection(
  workspace: string,
  scope: SkillScope,
  counts: WorkspaceCounts | null,
): "written" | "unchanged" | null {
  const target = path.join(workspace, CLAUDE_FILE);
  if (!fs.existsSync(target)) return null;
  return writeIfChanged(target, withCheckSection(fs.readFileSync(target, "utf8"), renderCheckSection(scope, counts)));
}

/** True where the workspace's CLAUDE.md holds our section. */
export function hasClaudeSection(workspace: string): boolean {
  const target = path.join(workspace, CLAUDE_FILE);
  return fs.existsSync(target) && span(fs.readFileSync(target, "utf8")) !== null;
}

/** Take our section out of CLAUDE.md. The file stays, even where it is left empty. */
export function removeClaudeSection(workspace: string): "section removed" | null {
  const target = path.join(workspace, CLAUDE_FILE);
  if (!fs.existsSync(target)) return null;
  const cleaned = withoutCheckSection(fs.readFileSync(target, "utf8"));
  if (!cleaned.changed) return null;
  fs.writeFileSync(target, cleaned.body);
  return "section removed";
}
