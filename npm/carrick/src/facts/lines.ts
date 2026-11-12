// The fact lines for an edit: which rows of the fact file the edit touched,
// found by name in the file's current text, printed verbatim from each row's
// `text` (the cloud writes the sentence; this package adds none).
//
// A row is found by (repo, name): the named things the changed lines sit in
// (`match.ts`). Where two files in one repo hold rows under the same name, the
// edited file decides; where only one does and it is another file that still
// declares that name, the edit is to a different thing and nothing prints. A
// row with no name (an inline handler or a call at module top level) is found
// by the operation string written in the call the edit reached: its `site`
// when the file states one, else the last segment of the label's path. When
// that string is not unique in the file, nothing prints.
//
// Each row is said once per session, claimed only once delivered (the caller
// calls `markFacts` after printing). Never throws.

import fs from "node:fs";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { createLogger } from "../log.ts";
import { factsSaid } from "../hook/reuse.ts";
import { linkedCheckoutTop } from "../hook/worktree.ts";
import { rangesOf, rangesOfLines, scanSource, touched } from "./match.ts";
import type { CallSpan, Scan } from "./match.ts";
import { FACTS_STALE_MS, openFacts } from "./store.ts";
import type { FactRow, FactStore, LocalRepo } from "./store.ts";

const log = createLogger("carrick-facts");

/** At most this many fact lines for one action. */
export const MAX_FACT_LINES = 3;

/**
 * One edited file and where the edit changed it: the 1-based lines of the
 * file as it is now, else the text the edit put in; both null means the file
 * was written whole.
 */
export type FactEdit = { file: string; lines?: number[] | null; snippets?: string[] | null };

/** The files the declaration scanner reads. */
const SCANNED = /\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)$/;

export type FactOutcome = {
  lines: string[];
  /** Row key -> delivery stamp and the absolute paths the line pointed at; claim with `markFacts` once printed. */
  claims: Record<string, { at: number; named: string[] }>;
  /** Time spent opening the fact file and matching. */
  ms: number;
  /** The fact file is missing, unreadable or older than `FACTS_STALE_MS`: start a background refresh. */
  stale: boolean;
};

/** The main checkout a linked worktree belongs to, from its `.git` file. */
function mainCheckoutOf(worktreeTop: string): string | null {
  try {
    const pointer = fs.readFileSync(path.join(worktreeTop, ".git"), "utf8").match(/^gitdir:\s*(.+)$/m)?.[1]?.trim();
    if (!pointer) return null;
    const gitdir = path.resolve(worktreeTop, pointer);
    let common: string | null = null;
    try {
      common = path.resolve(gitdir, fs.readFileSync(path.join(gitdir, "commondir"), "utf8").trim());
    } catch {
      const at = gitdir.lastIndexOf(`${path.sep}worktrees${path.sep}`);
      common = at >= 0 ? gitdir.slice(0, at) : null;
    }
    return common ? path.dirname(common) : null;
  } catch {
    return null;
  }
}

function isJsx(file: string): boolean {
  return /\.(tsx|jsx)$/.test(file);
}

function real(dir: string): string {
  try {
    return fs.realpathSync(dir);
  } catch {
    return dir;
  }
}

/** The repo `file` belongs to, its path in that repo, and the checkout it sits in. */
export function locate(file: string, repos: readonly LocalRepo[]): { repo: string; rel: string; top: string } | null {
  const top = linkedCheckoutTop(file);
  if (top !== null) {
    const main = mainCheckoutOf(top);
    const found = main === null ? undefined : repos.find((entry) => real(entry.path) === real(main));
    return found ? { repo: found.repo, rel: path.relative(top, real(file)), top } : null;
  }
  let best: LocalRepo | null = null;
  const abs = real(file);
  for (const entry of repos) {
    const base = real(entry.path);
    if ((abs === base || abs.startsWith(`${base}${path.sep}`)) && (best === null || base.length > real(best.path).length)) best = entry;
  }
  return best ? { repo: best.repo, rel: path.relative(real(best.path), abs), top: real(best.path) } : null;
}

/** The last segment of an operation string: a path's last part, or the whole event or topic name. */
function lastSegment(value: string): string {
  return value.includes("/") ? (value.split("/").filter(Boolean).pop() ?? "") : value;
}

/** The label's operation string with any dispatch case dropped (`POST /events {type=refund}` -> `/events`). */
function labelTail(label: string): string {
  return label.replace(/ \{[^}]*\}$/, "").split(" ").pop() ?? "";
}

/** The rows with no name that the reached calls name by one string unique in the file. */
function unnamedRows(rows: FactRow[], reached: CallSpan[], fileCalls: CallSpan[]): FactRow[] {
  const out: FactRow[] = [];
  const pick = (key: (row: FactRow) => string | null, holds: (literal: string, wanted: string) => boolean): FactRow[] => {
    const found: FactRow[] = [];
    for (const row of rows) {
      const wanted = key(row);
      if (!wanted) continue;
      if (rows.filter((other) => key(other) === wanted).length !== 1) continue;
      const callsHolding = fileCalls.filter((call) => call.literals.some((literal) => holds(literal, wanted)));
      if (callsHolding.length !== 1 || !reached.includes(callsHolding[0]!)) continue;
      found.push(row);
    }
    return found;
  };
  const bySite = pick((row) => row.site, (literal, wanted) => literal === wanted);
  out.push(...bySite);
  const rest = rows.filter((row) => !bySite.includes(row));
  const tail = (row: FactRow): string | null => {
    const segment = lastSegment(labelTail(row.label));
    return segment || null;
  };
  for (const row of pick(tail, (literal, wanted) => lastSegment(literal) === wanted)) if (rest.includes(row)) out.push(row);
  return out;
}

/**
 * The rows under `name` that are this file's thing. The caller has made sure
 * the edited file declares `name` exactly once. A row in the edited file is
 * its own; a row in another file counts only when every row under the name
 * sits in that one file and the file is gone from both the edit's checkout
 * and the repo's checkout (the thing moved here). A row file that is still
 * on disk, whatever it now declares, is another thing.
 */
function namedRows(store: FactStore, repo: string, name: string, rel: string, checkouts: readonly string[]): FactRow[] {
  const rows = store.byName(repo, name);
  if (rows.length === 0) return [];
  const files = new Set(rows.map((row) => row.file));
  if (files.has(rel)) return rows.filter((row) => row.file === rel);
  if (files.size > 1) return [];
  const other = rows[0]!.file;
  if (checkouts.some((top) => fs.existsSync(path.join(top, other)))) return [];
  return rows;
}

function rowKey(row: FactRow): string {
  return `${row.repo}::${row.name ?? row.file}::${row.side}::${row.label}`;
}

/** Open the fact file and the lines for `edits`. */
export async function factLines(
  root: string,
  edits: readonly FactEdit[],
  session: string | undefined,
  options: { home?: string; now?: number } = {},
): Promise<FactOutcome> {
  const started = performance.now();
  const now = options.now ?? Date.now();
  const lines: string[] = [];
  const claims: FactOutcome["claims"] = {};
  let stale = true;
  const done = (): FactOutcome => ({ lines, claims, stale, ms: Math.round((performance.now() - started) * 10) / 10 });
  let store: FactStore | null = null;
  try {
    store = await openFacts(root);
    if (store === null) return done();
    const fetched = Date.parse(store.meta["fetched_at"] ?? "");
    stale = !Number.isFinite(fetched) || now - fetched > FACTS_STALE_MS;
    const said = session ? factsSaid(session, options.home) : new Set<string>();
    const where = new Map(store.repos.map((entry) => [entry.repo, entry.path]));
    for (const edit of edits) {
      if (lines.length >= MAX_FACT_LINES) break;
      if (!SCANNED.test(edit.file)) continue;
      const at = locate(edit.file, store.repos);
      if (at === null) continue;
      let text: string;
      try {
        text = fs.readFileSync(edit.file, "utf8");
      } catch {
        continue;
      }
      const scan: Scan = scanSource(text, isJsx(edit.file));
      if (!scan.ok) {
        log(`${at.rel} did not read cleanly; no fact lines for it`);
        continue;
      }
      const ranges = edit.lines
        ? rangesOfLines(text, edit.lines)
        : edit.snippets
          ? rangesOf(text, edit.snippets)
          : [{ start: 0, end: text.length }];
      if (ranges.length === 0) continue;
      const reach = touched(text, scan, ranges);
      const rows: FactRow[] = [];
      // A name declared twice in this file (a handler of one name in two
      // components) cannot say which one the edit reached.
      const declared = new Map<string, number>();
      for (const span of scan.spans) declared.set(span.name, (declared.get(span.name) ?? 0) + 1);
      const checkouts = [at.top, where.get(at.repo)].filter((top): top is string => typeof top === "string");
      for (const name of reach.names) {
        if (declared.get(name) !== 1) continue;
        rows.push(...namedRows(store, at.repo, name, at.rel, checkouts));
      }
      if (reach.calls.length > 0) {
        const unnamed = store.unnamed(at.repo, at.rel);
        if (unnamed.length > 0) {
          const fileCalls = touched(text, scan, [{ start: 0, end: text.length }]).calls;
          rows.push(...unnamedRows(unnamed, reach.calls, fileCalls));
        }
      }
      for (const row of rows) {
        if (lines.length >= MAX_FACT_LINES) break;
        const key = rowKey(row);
        if (said.has(key) || key in claims) continue;
        lines.push(row.text);
        const named: string[] = [];
        for (const other of row.others) {
          const base = where.get(other.repo);
          if (base) named.push(path.join(base, other.file));
        }
        claims[key] = { at: now, named };
      }
    }
  } catch (error) {
    log("fact lines failed", String(error));
  } finally {
    store?.close();
  }
  return done();
}
