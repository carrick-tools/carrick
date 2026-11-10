// The line an edit gets when a function it added looks like one the index
// already holds (carrick#3249). The binary decides (`recheck.new_functions[].lead`,
// `docs/local-mode-output.md`); this renders it, once per function per session.

import path from "node:path";
import type { CheckResult, Lead } from "../contract.ts";

/** What one answer's leads need to print: the lines, and the keys to claim on delivery. */
export type LeadLines = { lines: string[]; keys: string[] };

/** The session key of a lead: the edited file and the new function's name. */
export function leadKey(result: CheckResult, relative: string, name: string): string {
  return `lead::${result.repo ?? ""}::${result.file ?? relative}::${name}`;
}

function where(lead: Lead, ownRepo: string | null): string {
  // `lead.repo` is a checkout folder name or an indexed repo name, never a path.
  const spot = `${lead.file}:${lead.line}`;
  return ownRepo !== null && lead.repo !== ownRepo ? `${lead.repo}/${spot}` : spot;
}

/**
 * One line for each new function of `result` that carries a lead and has not
 * been shown (`isNew`). The score and basis go to the log, never into the text.
 */
export function leadLines(
  result: CheckResult,
  relative: string,
  isNew: (key: string) => boolean,
  log: (...parts: string[]) => void = () => {},
): LeadLines {
  const out: LeadLines = { lines: [], keys: [] };
  if (result.error) return out;
  const ownRepo = result.repo ? path.basename(result.repo) : null;
  for (const made of result.recheck?.new_functions ?? []) {
    const lead = made.lead;
    if (!lead) continue;
    const key = leadKey(result, relative, made.name);
    if (out.keys.includes(key) || !isNew(key)) continue;
    const callers = lead.callers.map((caller) => `${caller.file}:${caller.line}`);
    const more = lead.callers_total - callers.length;
    const calledFrom =
      callers.length === 0 ? "" : ` (called from ${callers.join(", ")}${more > 0 ? `, +${more} more` : ""})`;
    out.lines.push(
      `Carrick: \`${made.name}\` looks like \`${lead.name}\` at ${where(lead, ownRepo)}${calledFrom}: reuse or extend it if it does the same job.`,
    );
    out.keys.push(key);
    log(`lead ${relative}::${made.name} -> ${lead.file}:${lead.line} basis=${lead.basis} score=${lead.score}`);
  }
  return out;
}
