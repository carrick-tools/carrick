// A function the edit added whose name is already declared in another file
// (`k: "name"` rows). Lookup only: the same name, never a likeness.
//
// A name is added when the file now declares it exactly once and the file
// before the edit did not declare it at all. The file before is rebuilt from
// the file now and the patch (its added lines dropped, its removed text put
// back); a file the call created had nothing before. With neither, nothing is
// said. A name shorter than 4 characters, or with a row in this very file,
// says nothing; otherwise the first of up to 3 rows by (repo, file) whose file
// still scans cleanly and declares exactly that name on the stated line
// prints. A row in a repo not checked out here is stated from the index.

import path from "node:path";
import type { Candidate } from "../lines.ts";
import { MAX_FACT_FILE_BYTES } from "../files.ts";
import type { FileMemo } from "../files.ts";
import { scanSource } from "../match.ts";
import type { Scan } from "../match.ts";
import type { FactStore, NameRow } from "../store.ts";

/** A name shorter than this (its last dotted part) is too common to say anything about. */
export const MIN_NAME_LENGTH = 4;

/** At most this many other declarations of one name are tried, in (repo, file) order. */
const MAX_NAME_ROWS = 3;

/** The edit as `facts/lines.ts` holds it, for this rule. */
type Change = { lines?: number[] | null; removed?: number[] | null; removedText?: string[] | null; whole?: boolean };

/**
 * The text before the edit: `text` (the file now) with the `added` lines
 * dropped and each removed line put back just above the line `removed`
 * names. Null when the removed lines and their text do not pair up, or when
 * the text before would be larger than `MAX_FACT_FILE_BYTES` (R33.5).
 */
export function textBefore(text: string, added: readonly number[], removed: readonly number[], removedText: readonly string[]): string | null {
  if (removed.length !== removedText.length) return null;
  let size = text.length;
  for (const line of removedText) size += line.length + 1;
  if (size > MAX_FACT_FILE_BYTES) return null;
  const now = text.split("\n");
  const drop = new Set(added);
  const before: string[] = [];
  let next = 0;
  for (let line = 1; line <= now.length + 1; line += 1) {
    while (next < removed.length && removed[next] === line) before.push(removedText[next++]!);
    if (line <= now.length && !drop.has(line)) before.push(now[line - 1]!);
  }
  if (next !== removed.length) return null;
  const joined = before.join("\n");
  return Buffer.byteLength(joined, "utf8") > MAX_FACT_FILE_BYTES ? null : joined;
}

/**
 * Whether `word` appears in `text` as a whole identifier: not inside a longer
 * one (Unicode identifier characters, `$`, and the joiners count as part of one).
 */
function holdsWord(text: string, word: string): boolean {
  const escaped = word.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(?<![\\p{ID_Continue}$\\u200c\\u200d])${escaped}(?![\\p{ID_Continue}$\\u200c\\u200d])`, "u").test(text);
}

/**
 * The names the edit added: declared once now, and absent before (neither
 * declared nor written anywhere as a whole word: `const f = wrap(...)`,
 * `module.exports = { f() {} }` rewritten as `function f` is no new
 * function); null when the text before cannot be known.
 */
function addedNames(names: readonly string[], text: string, change: Change, jsx: boolean): string[] | null {
  let before: Set<string>;
  let old = "";
  if (change.whole) before = new Set();
  else {
    if (!change.lines || !change.removed || !change.removedText) return null;
    const rebuilt = textBefore(text, change.lines, change.removed, change.removedText);
    if (rebuilt === null) return null;
    const oldScan = scanSource(rebuilt, jsx);
    if (!oldScan.ok) return null;
    before = new Set(oldScan.spans.map((span) => span.name));
    old = rebuilt;
  }
  return names.filter((name) => !before.has(name) && !holdsWord(old, name.slice(name.lastIndexOf(".") + 1)));
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
  memo: FileMemo,
): Candidate[] {
  // First the store (R33.5): the names declared once in the file now that
  // another file declares. Only when some remain is the text before the edit
  // rebuilt and scanned.
  const count = new Map<string, number>();
  for (const span of scan.spans) count.set(span.name, (count.get(span.name) ?? 0) + 1);
  const rowsOf = new Map<string, NameRow[]>();
  for (const [name, n] of count) {
    if (n !== 1 || name.slice(name.lastIndexOf(".") + 1).length < MIN_NAME_LENGTH) continue;
    const rows = store.declarations(name);
    // A row in this file: the name was here on main, so it is not a new one.
    if (rows.length === 0 || rows.some((row) => row.repo === at.repo && row.file === at.rel)) continue;
    rowsOf.set(name, rows);
  }
  if (rowsOf.size === 0) return [];
  const names = addedNames([...rowsOf.keys()], text, change, jsx);
  if (names === null) return [];
  const out: Candidate[] = [];
  for (const name of names) {
    // Up to `MAX_NAME_ROWS` rows in (repo, file) order, under one key: the
    // first whose check passes prints, and the rest are skipped (R33.7, F8).
    for (const row of rowsOf.get(name)!.slice(0, MAX_NAME_ROWS)) {
      const base = row.repo === at.repo ? at.top : where.get(row.repo);
      // A repo not checked out here: stated from the index, which it cannot
      // have changed since (R33.1).
      if (!base) {
        out.push({ key: `name::${name}`, text: row.text, named: [] });
        continue;
      }
      const other = path.join(base, row.file);
      // The other file must still declare exactly this name, read by the same
      // scanner, starting on the line the row states (R33.3): gone, rewritten,
      // only importing it, moved within the file, or not scanning cleanly, the
      // line would be wrong or the function may have moved into this file.
      // Checked only for a line about to print.
      const holds = (): boolean =>
        memo.scanned(other)?.scan.spans.some((span) => span.name === name && memo.lineOf(other, span.start) === row.line) ?? false;
      out.push({ key: `name::${name}`, text: row.text, named: [other], holds });
    }
  }
  return out;
}
