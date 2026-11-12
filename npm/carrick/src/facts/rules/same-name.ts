// A function the edit added whose name is already declared in another file
// (`k: "name"` rows). Lookup only: the same name, never a likeness.
//
// A name is added when the file now declares it exactly once and the file
// before the edit did not declare it at all. The file before is rebuilt from
// the file now and the patch (its added lines dropped, its removed text put
// back); a file the call created had nothing before. With neither, nothing is
// said. A name shorter than 4 characters, or with a row in this very file,
// says nothing; otherwise the first row by (repo, file) prints, and only when
// its file still scans cleanly and declares exactly that name.

import path from "node:path";
import { scanFile } from "../lines.ts";
import type { Candidate } from "../lines.ts";
import { scanSource } from "../match.ts";
import type { Scan } from "../match.ts";
import type { FactStore } from "../store.ts";

/** A name shorter than this (its last dotted part) is too common to say anything about. */
export const MIN_NAME_LENGTH = 4;

/** The edit as `facts/lines.ts` holds it, for this rule. */
type Change = { lines?: number[] | null; removed?: number[] | null; removedText?: string[] | null; whole?: boolean };

/**
 * The text before the edit: `text` (the file now) with the `added` lines
 * dropped and each removed line put back just above the line `removed`
 * names. Null when the removed lines and their text do not pair up.
 */
export function textBefore(text: string, added: readonly number[], removed: readonly number[], removedText: readonly string[]): string | null {
  if (removed.length !== removedText.length) return null;
  const now = text.split("\n");
  const drop = new Set(added);
  const before: string[] = [];
  let next = 0;
  for (let line = 1; line <= now.length + 1; line += 1) {
    while (next < removed.length && removed[next] === line) before.push(removedText[next++]!);
    if (line <= now.length && !drop.has(line)) before.push(now[line - 1]!);
  }
  return next === removed.length ? before.join("\n") : null;
}

/** The names the edit added: declared once now, not at all before; null when that cannot be known. */
function addedNames(text: string, scan: Scan, change: Change, jsx: boolean): string[] | null {
  let before: Set<string>;
  if (change.whole) before = new Set();
  else {
    if (!change.lines || !change.removed || !change.removedText) return null;
    const old = textBefore(text, change.lines, change.removed, change.removedText);
    if (old === null) return null;
    const oldScan = scanSource(old, jsx);
    if (!oldScan.ok) return null;
    before = new Set(oldScan.spans.map((span) => span.name));
  }
  const count = new Map<string, number>();
  for (const span of scan.spans) count.set(span.name, (count.get(span.name) ?? 0) + 1);
  return [...count].filter(([name, n]) => n === 1 && !before.has(name)).map(([name]) => name);
}

/** The `name` lines for one edited file: at most one per added name. */
export function sameNameLines(
  store: FactStore,
  at: { repo: string; rel: string; top: string },
  where: ReadonlyMap<string, string>,
  text: string,
  scan: Scan,
  change: Change,
  jsx: boolean,
): Candidate[] {
  const names = addedNames(text, scan, change, jsx);
  if (names === null) return [];
  const out: Candidate[] = [];
  for (const name of names) {
    const bare = name.slice(name.lastIndexOf(".") + 1);
    if (bare.length < MIN_NAME_LENGTH) continue;
    const rows = store.declarations(name);
    // A row in this file: the name was here on main, so it is not a new one.
    if (rows.some((row) => row.repo === at.repo && row.file === at.rel)) continue;
    const first = rows[0];
    if (!first) continue;
    const base = first.repo === at.repo ? at.top : where.get(first.repo);
    if (!base) continue;
    const other = path.join(base, first.file);
    // The other file must still declare exactly this name, read by the same
    // scanner: gone, rewritten, only importing it, or not scanning cleanly,
    // the function may have moved into this file. Checked only for a line
    // about to print.
    const holds = (): boolean => scanFile(other)?.spans.some((span) => span.name === name) ?? false;
    out.push({ key: `name::${name}`, text: first.text, named: [other], holds });
  }
  return out;
}
