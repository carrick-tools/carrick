// What an edit gets when a function it added may already exist in the index
// (carrick-cloud#3490). The binary decides (`recheck.new_functions[].similar`,
// `docs/local-mode-output.md`); this renders it, once per function per session.
// The wording is frozen with the measured block, so a change to it is a change
// to what an agent was measured on.

import path from "node:path";
import type { CheckResult, SimilarFunction } from "../contract.ts";
import { fileOf, isAdded } from "./added.ts";
import type { Added } from "./added.ts";

/** What one answer's similar blocks need to print: the blocks, and the keys to claim on delivery. */
export type SimilarLines = { lines: string[]; keys: string[] };

/** Longest signature and intent a row prints. */
const SIGNATURE_CAP = 160;
const INTENT_CAP = 200;

/** The session key of a new function: the edited file and its name. */
export function similarKey(result: CheckResult, relative: string, name: string): string {
  return `lead::${result.repo ?? ""}::${result.file ?? relative}::${name}`;
}

/**
 * Where a row is, read from the agent's folder `cwd`. `row.repo` is a checkout
 * folder name or an indexed repo name, never a path; `ownRoot` is the edited
 * repo's root. A row in another repo is named, and one whose repo has no
 * checkout here says so. Without a `cwd` the text is the older repo-relative one.
 */
function where(row: SimilarFunction, ownRepo: string | null, ownRoot: string | null, cwd: string | null): string {
  const other = ownRepo !== null && row.repo !== ownRepo;
  if (cwd === null || ownRoot === null) return other ? `${row.repo}/${row.file}:${row.line}` : `${row.file}:${row.line}`;
  if (!other) return `${path.relative(cwd, path.join(ownRoot, row.file))}:${row.line}`;
  if (row.checkout === undefined) {
    return `${row.file}:${row.line} in repo ${row.repo}, which is not checked out on this machine`;
  }
  return `${path.relative(cwd, path.join(row.checkout, row.file))}:${row.line} in repo ${row.repo}`;
}

/**
 * One line: every run of whitespace (newlines included) becomes one space, then
 * the first `cap` characters, never splitting a surrogate pair; a cut text ends
 * in "...".
 */
function cut(text: string, cap: number): string {
  const chars = [...text.replace(/\s+/g, " ").trim()];
  return chars.length <= cap ? chars.join("") : `${chars.slice(0, cap).join("")}...`;
}

function row(position: number, found: SimilarFunction, place: string): string {
  const signature = found.signature ? cut(found.signature, SIGNATURE_CAP) : "";
  const first =
    `${position}. \`${found.name}${signature}\` at ${place} ` +
    `(${found.exported ? "exported" : "not exported"}; called from ${found.called_from} other files)`;
  return found.intent ? `${first}\n   ${cut(found.intent, INTENT_CAP)}` : first;
}

/**
 * One block for each function this edit added (`added`, `isAdded`: the
 * binary's list also holds functions the index is merely behind on) that has
 * similar rows and has not been shown (`isNew`). The score goes to the log,
 * never into the text.
 */
export function similarLines(
  result: CheckResult,
  relative: string,
  cwd: string | null,
  isNew: (key: string) => boolean,
  log: (...parts: string[]) => void = () => {},
  added?: Added,
): SimilarLines {
  const out: SimilarLines = { lines: [], keys: [] };
  if (result.error) return out;
  const ownRepo = result.repo ? path.basename(result.repo) : null;
  const ownRoot = result.repo ?? null;
  for (const made of result.recheck?.new_functions ?? []) {
    const rows = made.similar ?? [];
    if (rows.length === 0) continue;
    if (added === undefined && ownRoot === null) continue;
    if (!isAdded(added, fileOf(ownRoot ?? "", result.file ?? relative), made.name, made.line)) continue;
    const key = similarKey(result, relative, made.name);
    if (out.keys.includes(key) || !isNew(key)) continue;
    out.lines.push(
      [
        `Carrick: \`${made.name}\` may already exist. Existing functions that could do this job:`,
        ...rows.map((found, at) => row(at + 1, found, where(found, ownRepo, ownRoot, cwd))),
        "If one does the job, call it (export or move it if needed) and delete yours. If none does, keep yours.",
      ].join("\n"),
    );
    out.keys.push(key);
    log(
      `similar ${relative}::${made.name} -> ${rows.map((found) => `${found.file}:${found.line}=${found.score}`).join(" ")}`,
    );
  }
  return out;
}
