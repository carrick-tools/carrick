// What an edit gets when a function it added may already exist in the index
// (carrick-cloud#3490). The binary decides (`recheck.new_functions[].similar`,
// `docs/local-mode-output.md`); this renders it, once per function per session.
// The wording is frozen with the measured block, so a change to it is a change
// to what an agent was measured on.

import path from "node:path";
import type { CheckResult, SimilarFunction } from "../contract.ts";

/** What one answer's similar blocks need to print: the blocks, and the keys to claim on delivery. */
export type SimilarLines = { lines: string[]; keys: string[] };

/** Longest signature and intent a row prints. */
const SIGNATURE_CAP = 160;
const INTENT_CAP = 200;

/** The session key of a new function: the edited file and its name. */
export function similarKey(result: CheckResult, relative: string, name: string): string {
  return `lead::${result.repo ?? ""}::${result.file ?? relative}::${name}`;
}

function where(row: SimilarFunction, ownRepo: string | null): string {
  // `row.repo` is a checkout folder name or an indexed repo name, never a path.
  const spot = `${row.file}:${row.line}`;
  return ownRepo !== null && row.repo !== ownRepo ? `${row.repo}/${spot}` : spot;
}

/** The first `cap` characters, never splitting a surrogate pair. */
function cut(text: string, cap: number): string {
  const chars = [...text];
  return chars.length <= cap ? text : chars.slice(0, cap).join("");
}

function row(position: number, found: SimilarFunction, ownRepo: string | null): string {
  const signature = found.signature ? cut(found.signature, SIGNATURE_CAP) : "";
  const first =
    `${position}. \`${found.name}${signature}\` at ${where(found, ownRepo)} ` +
    `(${found.exported ? "exported" : "not exported"}; called from ${found.called_from} other files)`;
  return found.intent ? `${first}\n   ${cut(found.intent, INTENT_CAP)}` : first;
}

/**
 * One block for each new function of `result` that has similar rows and has not
 * been shown (`isNew`). The score goes to the log, never into the text.
 */
export function similarLines(
  result: CheckResult,
  relative: string,
  isNew: (key: string) => boolean,
  log: (...parts: string[]) => void = () => {},
): SimilarLines {
  const out: SimilarLines = { lines: [], keys: [] };
  if (result.error) return out;
  const ownRepo = result.repo ? path.basename(result.repo) : null;
  for (const made of result.recheck?.new_functions ?? []) {
    const rows = made.similar ?? [];
    if (rows.length === 0) continue;
    const key = similarKey(result, relative, made.name);
    if (out.keys.includes(key) || !isNew(key)) continue;
    out.lines.push(
      [
        `Carrick: \`${made.name}\` may already exist. Existing functions that could do this job:`,
        ...rows.map((found, at) => row(at + 1, found, ownRepo)),
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
