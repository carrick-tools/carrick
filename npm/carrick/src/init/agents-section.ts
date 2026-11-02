// The "Carrick check" section `carrick init` keeps in a folder's AGENTS.md
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
// A second block, `carrick:rules`, sits right after it and tells the agent
// which Carrick tool to call at which decision point (the rules the hosted
// scaffold tool used to hand out). It has its own marker pair, so it can be
// deleted on its own and the file without it is today's file byte for byte.
//
// carrick-cloud's scaffold (cloud#2289) must write this same block.

import fs from "node:fs";
import path from "node:path";

import { writeIfChanged } from "./files.ts";
import { scopeArgument, type SkillScope } from "./task-skills.ts";

export const AGENTS_FILE = "AGENTS.md";
/** Claude Code reads this one and skips AGENTS.md where it exists (unless it imports it). */
export const CLAUDE_FILE = "CLAUDE.md";

/** A hand-written "## Carrick" heading: somebody's own section, never ours to sit beside. */
const CARRICK_HEADING = /^##\s+Carrick\s*$/m;

/** True where a body holds a Carrick section of its own, markers or not. */
export function ownCarrickHeading(body: string): boolean {
  return CARRICK_HEADING.test(body);
}
export { CARRICK_HEADING };

const START = "<!-- carrick:check start -->";
const END = "<!-- carrick:check end -->";
export const RULES_START = "<!-- carrick:rules start -->";
export const RULES_END = "<!-- carrick:rules end -->";

/** The section as written, markers included. Ends in one newline. */
export const CHECK_SECTION = [
  START,
  "## Carrick check",
  "",
  "Before you say a change is done, run `carrick check --changed` alongside the tests. The per-edit check covers only the file each edit touched; `check --changed` checks every changed file on this branch against the other services. It can take a few minutes: allow up to five. Then tell the user in one line what Carrick checked and found (for example \"Carrick checked 3 changed files against 35 services: no contract problems\"). If it exits non-zero, fix what it reports, or say why it is wrong. When you added functions, use the carrick-reuse skill on them.",
  END,
  "",
].join("\n");

type Markers = { start: string; end: string };
const CHECK: Markers = { start: START, end: END };
const RULES: Markers = { start: RULES_START, end: RULES_END };

/** The span of a marked block in a body, end included; null where there is none. */
function spanOf(body: string, markers: Markers): { from: number; to: number } | null {
  const from = body.indexOf(markers.start);
  if (from === -1) return null;
  const end = body.indexOf(markers.end, from);
  if (end === -1) return null;
  const after = end + markers.end.length;
  return { from, to: body[after] === "\n" ? after + 1 : after };
}

function span(body: string): { from: number; to: number } | null {
  return spanOf(body, CHECK);
}

/** The body with our section written in: replaced where it is, appended where it is not. */
export function withCheckSection(existing: string | null): string {
  if (existing === null || existing.trim() === "") return CHECK_SECTION;
  const found = span(existing);
  if (found !== null) return existing.slice(0, found.from) + CHECK_SECTION + existing.slice(found.to);
  const lf = existing.endsWith("\n") ? existing : `${existing}\n`;
  return `${lf}\n${CHECK_SECTION}`;
}

/** A block taken out of a body, with the blank line `with*Section` put before it. */
function without(existing: string, markers: Markers): { body: string; changed: boolean; empty: boolean } {
  const found = spanOf(existing, markers);
  if (found === null) return { body: existing, changed: false, empty: false };
  let from = found.from;
  const before = existing.slice(0, from);
  if (/\n\n$/.test(before)) from -= 1;
  const body = existing.slice(0, from) + existing.slice(found.to);
  return { body, changed: true, empty: body.trim() === "" };
}

/** The body with our section taken out; `empty` where nothing else was in it. */
export function withoutCheckSection(existing: string): { body: string; changed: boolean; empty: boolean } {
  return without(existing, CHECK);
}

/** The body with the rules block taken out; `empty` where nothing else was in it. */
export function withoutRulesSection(existing: string): { body: string; changed: boolean; empty: boolean } {
  return without(existing, RULES);
}

/**
 * The rules block for one scope, markers included. Ends in one newline.
 *
 * The body is the decision-point text the hosted scaffold tool carried (the
 * wording measured on the activation benchmark, so the receipt line keeps its
 * ASCII arrow), with the project named the way the task skills name it and one
 * line added for the moment of changing an operation.
 */
export function rulesSection(scope: SkillScope): string {
  const named = scope.slug !== null;
  const project = named ? `the project **${scope.slug}**` : "this project";
  const pass = named
    ? `Pass \`${scopeArgument(scope)}\` (or \`repo: "<owner/repo>"\` from the git remote) on every call.`
    : `Pass \`${scopeArgument(scope)}\` (read it with \`git remote get-url origin\`) on every call.`;
  return [
    RULES_START,
    "## Carrick: use it while you work",
    "",
    "**If the answer is defined by another service \u2014 its endpoints, its real request/response types, who consumes it, or any local type, constant, or SDK helper that mirrors it \u2014 ask Carrick before you rely on anything local.** Local copies of another service's contract drift and lie; the producing service defines the contract, and if two definitions disagree, the producer's definition wins. Load the Carrick tools now so they are ready at that moment.",
    "",
    "**Read the file you are editing. For anything that lives elsewhere in the workspace (a helper, an endpoint, a consumer, a type), ask Carrick first, then grep to confirm a location.** Run `search_by_intent` before writing any helper, parser, validator, or domain function, even when you are sure it is new. Grep can prove a name is absent; it can never prove the behaviour is.",
    "",
    "Before you change an operation (an HTTP route, a GraphQL field, a socket event or a pub/sub topic), a request or response shape, or an exported function, call `get_operation` or `get_callers` and update or clear every consumer it lists, in every repo.",
    "",
    "Then, before you write new code, state one line naming what you asked and what came back:",
    "",
    "`Carrick checked: <query> -> <result count>`",
    "",
    "Start a session with `get_project_map`, passing your own service so the map splits this repo from its siblings: one cheap call that names the sibling services this project indexes, the contracts crossing into and out of this repo, the calls that reach no route, and the drill-down tool for each section.",
    "",
    "`get_endpoint_types` returns another service's real request and response types. Before adding or bumping an npm package, `get_service_dependencies` shows version conflicts across services. For audit-shaped work, such as listing what a service calls outside itself, `list_external_calls` names each recorded outbound call with its file and line.",
    "",
    `Carrick indexes every service in ${project}: every function with an intent description, its dependencies, and API endpoints with their real request/response types, on each repo's main branch, exported or not. ${pass} Carrick is read-only; data reflects the most recent scan of each repo. The index at main is what production integrates against, so build against a sibling's contract from the index and merge \u2014 your unmerged local state does not need to be visible to Carrick.`,
    "",
    "Write correct, idiomatic, explicitly typed code; never contort code so the scanner can read it. If Carrick fails to extract something written normally, that is a Carrick bug to report, not a constraint to code around.",
    RULES_END,
    "",
  ].join("\n");
}

/**
 * The body with the rules block written in: replaced where it is, otherwise put
 * right after the check block, otherwise appended.
 */
export function withRulesSection(existing: string | null, scope: SkillScope): string {
  const block = rulesSection(scope);
  if (existing === null || existing.trim() === "") return block;
  const own = spanOf(existing, RULES);
  if (own !== null) return existing.slice(0, own.from) + block + existing.slice(own.to);
  const check = span(existing);
  if (check !== null) return `${existing.slice(0, check.to)}\n${block}${existing.slice(check.to)}`;
  const lf = existing.endsWith("\n") ? existing : `${existing}\n`;
  return `${lf}\n${block}`;
}

function writeInto(target: string, scope: SkillScope): "written" | "unchanged" {
  const existing = fs.existsSync(target) ? fs.readFileSync(target, "utf8") : null;
  const checked = withCheckSection(existing);
  // A file with a Carrick section of its own is theirs to keep: the check block
  // sits beside it, our rules do not (they would say the same thing twice).
  const blocked = existing !== null && ownCarrickHeading(existing);
  return writeIfChanged(target, blocked ? checked : withRulesSection(checked, scope));
}

/** Write our blocks into the folder's AGENTS.md. */
export function writeAgentsSection(workspace: string, scope: SkillScope): "written" | "unchanged" {
  return writeInto(path.join(workspace, AGENTS_FILE), scope);
}

/**
 * Write our blocks into the folder's CLAUDE.md, where there is one that does
 * not import AGENTS.md. Claude Code loads CLAUDE.md and skips AGENTS.md when it
 * exists, so the AGENTS.md blocks never reach it otherwise. Never creates the
 * file; null where nothing was to be written.
 */
export function writeClaudeSection(workspace: string, scope: SkillScope): "written" | "unchanged" | null {
  if (!claudeCarries(workspace)) return null;
  return writeInto(path.join(workspace, CLAUDE_FILE), scope);
}

/** True where the folder holds a CLAUDE.md that does not import AGENTS.md. */
export function claudeCarries(workspace: string): boolean {
  const target = path.join(workspace, CLAUDE_FILE);
  return fs.existsSync(target) && !/^\s*@AGENTS\.md\s*$/m.test(fs.readFileSync(target, "utf8"));
}

/** True where a folder's file (AGENTS.md unless named) holds a hand-written Carrick section. */
export function hasOwnCarrickSection(workspace: string, file: string = AGENTS_FILE): boolean {
  const target = path.join(workspace, file);
  return fs.existsSync(target) && ownCarrickHeading(fs.readFileSync(target, "utf8"));
}

/** True where the folder's file (AGENTS.md unless named) holds one of our blocks. */
export function hasAgentsSection(workspace: string, file: string = AGENTS_FILE): boolean {
  const target = path.join(workspace, file);
  if (!fs.existsSync(target)) return false;
  const body = fs.readFileSync(target, "utf8");
  return span(body) !== null || spanOf(body, RULES) !== null;
}

/** Take our blocks out. The file goes too only where nothing else was in it. */
export function removeAgentsSection(
  workspace: string,
  file: string = AGENTS_FILE,
): "section removed" | "file removed" | null {
  const target = path.join(workspace, file);
  if (!fs.existsSync(target)) return null;
  // The rules go first: they sit after the check block, and taking the later
  // block first leaves the earlier one exactly as it was written.
  const rules = withoutRulesSection(fs.readFileSync(target, "utf8"));
  const cleaned = withoutCheckSection(rules.body);
  if (!rules.changed && !cleaned.changed) return null;
  if (cleaned.body.trim() === "") {
    fs.rmSync(target);
    return "file removed";
  }
  fs.writeFileSync(target, cleaned.body);
  return "section removed";
}
