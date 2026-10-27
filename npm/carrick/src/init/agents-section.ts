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
// carrick-cloud's scaffold (cloud#2289) must write this same block.

import fs from "node:fs";
import path from "node:path";

import { writeIfChanged } from "./files.ts";

export const AGENTS_FILE = "AGENTS.md";

const START = "<!-- carrick:check start -->";
const END = "<!-- carrick:check end -->";

/** The section as written, markers included. Ends in one newline. */
export const CHECK_SECTION = [
  START,
  "## Carrick check",
  "",
  "Before you say a change is done, run `carrick check --changed` alongside the tests. The per-edit check covers only the file each edit touched; `check --changed` checks every changed file on this branch against the other services. It can take a few minutes: allow up to five. Then tell the user in one line what Carrick checked and found (for example \"Carrick checked 3 changed files against 35 services: no contract problems\"). If it exits non-zero, fix what it reports, or say why it is wrong. When you added functions, use the carrick-reuse skill on them.",
  END,
  "",
].join("\n");

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
export function withCheckSection(existing: string | null): string {
  if (existing === null || existing.trim() === "") return CHECK_SECTION;
  const found = span(existing);
  if (found !== null) return existing.slice(0, found.from) + CHECK_SECTION + existing.slice(found.to);
  const lf = existing.endsWith("\n") ? existing : `${existing}\n`;
  return `${lf}\n${CHECK_SECTION}`;
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
export function writeAgentsSection(workspace: string): "written" | "unchanged" {
  const target = path.join(workspace, AGENTS_FILE);
  const existing = fs.existsSync(target) ? fs.readFileSync(target, "utf8") : null;
  return writeIfChanged(target, withCheckSection(existing));
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
