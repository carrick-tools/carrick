// The fact lines for an edit: which rows of the fact file the edit touched,
// found by name in the file's current text, printed verbatim from each row's
// `text` (the cloud writes the sentence; this package adds none).
//
// A row is found by (repo, name): the named things the changed lines sit in
// (`match.ts`), declared once in the edited file. A row prints only for its
// own file, never for a file it might have moved from. A
// row with no name (an inline handler or a call at module top level) is found
// only by its `site`, written as a string argument of the call around the
// changed line, with an HTTP row's method as the callee's name; when that
// pair is not unique in the file, or the row has no site, nothing prints.
//
// Each row is said once per session, claimed only once delivered (the caller
// calls `markFacts` after printing). Never throws.

import fs from "node:fs";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { createLogger } from "../log.ts";
import { factsSaid } from "../hook/reuse.ts";
import { linkedCheckoutTop } from "../hook/worktree.ts";
import { gapsOfLines, rangesOf, rangesOfLines, scanSource, touched } from "./match.ts";
import type { CallSpan, Scan } from "./match.ts";
import { factsEnabled } from "./enabled.ts";
import { FACTS_STALE_MS, openFacts } from "./store.ts";
import { sameNameLines } from "./rules/same-name.ts";
import { isManifest, packageLines } from "./rules/packages.ts";

/** A file's text, or null when it is unreadable or over `MAX_FACT_FILE_BYTES`. */
function readSmall(file: string): string | null {
  try {
    return fs.statSync(file).size > MAX_FACT_FILE_BYTES ? null : fs.readFileSync(file, "utf8");
  } catch {
    return null;
  }
}
import type { FactRow, FactStore, LocalRepo } from "./store.ts";

const log = createLogger("carrick-facts");

/** At most this many fact lines for one action. */
export const MAX_FACT_LINES = 3;

/** A file larger than this gets no fact lines (and is not read). */
export const MAX_FACT_FILE_BYTES = 512 * 1024;

/**
 * One edited file and where the edit changed it: the 1-based lines of the
 * file as it is now that it added (`lines`) and the lines it removed lines
 * just above (`removed`), else the text the edit put in (`snippets`), else
 * `whole` for a file the call created (every named thing in it counts). With
 * none of them the changed lines are not known, and nothing is said.
 * `removedText` is the text of each `removed` line, in order, when the
 * patch's other lines were checked against the file: with it the file before
 * the edit can be rebuilt.
 */
export type FactEdit = {
  file: string;
  lines?: number[] | null;
  removed?: number[] | null;
  removedText?: string[] | null;
  snippets?: string[] | null;
  whole?: boolean;
};

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
  /**
   * The edited files (as given) in a repo a readable fact file for this
   * workspace maps: their callers come from the fact file, so the probe's
   * uses-count line is not printed for them (v1-agent's Q2 ruling).
   */
  covered: string[];
};

/** A line one rule found, before the cap: its once-per-session key and the paths it points at. */
type Candidate = { key: string; text: string; named: string[] };

/** The row kinds in the order their lines print, before the cap of `MAX_FACT_LINES`. */
const KINDS = ["op", "fn", "name", "pkg"] as const;

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
    const found = main === null ? [] : repos.filter((entry) => real(entry.path) === real(main));
    return found.length === 1 ? { repo: found[0]!.repo, rel: path.relative(top, real(file)), top } : null;
  }
  let best: LocalRepo | null = null;
  // Two repos on one checkout (a fact file built before `matchRepos` refused them): neither.
  let tied = false;
  const abs = real(file);
  for (const entry of repos) {
    const base = real(entry.path);
    if (!(abs === base || abs.startsWith(`${base}${path.sep}`))) continue;
    const length = best === null ? -1 : real(best.path).length;
    if (base.length > length) [best, tied] = [entry, false];
    else if (base.length === length) tied = true;
  }
  return best && !tied ? { repo: best.repo, rel: path.relative(real(best.path), abs), top: real(best.path) } : null;
}

/** An HTTP row's method, lower case (`POST /orders` -> `post`); null for other protocols. */
function methodOf(row: FactRow): string | null {
  return row.protocol === "http" ? (row.label.split(" ")[0] ?? "").toLowerCase() : null;
}

/**
 * Whether `call` is written as `row`'s site: the site as a direct string
 * argument, and an HTTP row's method as the callee's name. Any other
 * protocol's producer (a listener, subscriber, resolver) also needs a
 * function written in the call; its consumer (an emit, a publish) never
 * fits, as the row does not say the callee's name.
 */
function callFits(call: CallSpan, row: FactRow, method: string | null): boolean {
  if (!call.literals.includes(row.site!)) return false;
  if (method !== null) return call.member?.toLowerCase() === method;
  return row.side === "producer" && call.takesFunction;
}

/**
 * The rows with no name (or several under one name, `holdsSeveral`) that a
 * reached call writes: the row's `site` is a
 * direct string argument of the call around the changed line, an HTTP row's
 * method is the callee's name (`.post(` for `POST ...`), and that (site,
 * method) pair belongs to one row and one call in the file. A row with no
 * site never matches.
 */
function unnamedRows(rows: FactRow[], reached: readonly CallSpan[], fileCalls: readonly CallSpan[]): FactRow[] {
  const out: FactRow[] = [];
  const reachedSet = new Set(reached);
  for (const row of rows) {
    if (row.site === null) continue;
    const method = methodOf(row);
    if (rows.filter((other) => other.site === row.site && methodOf(other) === method).length !== 1) continue;
    const holding = fileCalls.filter((call) => callFits(call, row, method));
    if (holding.length === 1 && reachedSet.has(holding[0]!)) out.push(row);
  }
  return out;
}

/**
 * The rows under `name` that are this file's thing. The caller has made sure
 * the edited file declares `name` exactly once. Only a row in the edited file
 * is its own: a row in another file, even one gone from disk, is never
 * guessed to have moved here.
 */
function namedRows(store: FactStore, repo: string, name: string, rel: string): FactRow[] {
  return store.byName(repo, name).filter((row) => row.file === rel);
}

/**
 * Whether one name's rows in a file are several things (R24.1): rows grouped
 * by (side, protocol), and some group holds two or more (inline handlers
 * named by their encloser, `registerRoutes` with five `app.get`). Then every
 * row under the name, in every group, prints only when its own site call is
 * reached (`unnamedRows`); a row with no site, or none reached, is silent.
 * When every group holds one row (a resolver that also calls a route: one
 * thing, two facts) all of them print, as for any named thing.
 */
function holdsSeveral(rows: readonly FactRow[]): boolean {
  const seen = new Set<string>();
  for (const row of rows) {
    const group = `${row.side} ${row.protocol}`;
    if (seen.has(group)) return true;
    seen.add(group);
  }
  return false;
}

function rowKey(row: FactRow): string {
  return `${row.repo}::${row.name ?? row.file}::${row.side}::${row.label}`;
}

/** Open the fact file and the lines for `edits`. */
export async function factLines(
  root: string,
  edits: readonly FactEdit[],
  session: string | undefined,
  options: { home?: string; now?: number; env?: NodeJS.ProcessEnv } = {},
): Promise<FactOutcome> {
  const started = performance.now();
  const now = options.now ?? Date.now();
  const lines: string[] = [];
  const claims: FactOutcome["claims"] = {};
  const covered: string[] = [];
  let stale = true;
  const done = (): FactOutcome => ({ lines, claims, stale, covered, ms: Math.round((performance.now() - started) * 10) / 10 });
  let store: FactStore | null = null;
  const found: Record<(typeof KINDS)[number], Candidate[]> = { op: [], fn: [], name: [], pkg: [] };
  // Off by default (`enabled.ts`): the store is not opened and nothing refreshes.
  if (!factsEnabled(options.env)) {
    stale = false;
    return done();
  }
  try {
    store = await openFacts(root);
    if (store === null) return done();
    const facts: FactStore = store;
    const fetched = Date.parse(store.meta["fetched_at"] ?? "");
    stale = !Number.isFinite(fetched) || now - fetched > FACTS_STALE_MS;
    const said = session ? factsSaid(session, options.home) : new Set<string>();
    const where = new Map(store.repos.map((entry) => [entry.repo, entry.path]));
    const pathsOf = (others: ReadonlyArray<{ repo: string; file: string }>): string[] =>
      others.flatMap((other) => {
        const base = where.get(other.repo);
        return base ? [path.join(base, other.file)] : [];
      });
    // Each kind's lookup on its own: a row of one kind that throws (a field
    // ingest let through) loses only that kind's lines for the file.
    const isolated = (kind: (typeof KINDS)[number], lookup: () => Candidate[]): void => {
      try {
        found[kind].push(...lookup());
      } catch (error) {
        log(`${kind} fact lines failed`, String(error));
      }
    };
    for (const edit of edits) {
      const at = locate(edit.file, store.repos);
      if (at === null) continue;
      covered.push(edit.file);
      if (isManifest(edit.file)) {
        const manifest = readSmall(edit.file);
        if (manifest !== null) isolated("pkg", () => packageLines(facts, at, where, manifest, edit));
        continue;
      }
      if (!SCANNED.test(edit.file)) continue;
      const byLine = edit.lines || edit.removed;
      // Changed lines not known (a Write over a file, a shell write, a patch
      // hunk that cannot be placed): nothing is said.
      if (!byLine && !edit.snippets && !edit.whole) continue;
      let text: string;
      try {
        // A file this large is generated or bundled: never read in a hook's time.
        if (fs.statSync(edit.file).size > MAX_FACT_FILE_BYTES) continue;
        text = fs.readFileSync(edit.file, "utf8");
      } catch {
        continue;
      }
      const scan: Scan = scanSource(text, isJsx(edit.file));
      if (!scan.ok) {
        log(`${at.rel} did not read cleanly; no fact lines for it`);
        continue;
      }
      const ranges = byLine
        ? rangesOfLines(text, edit.lines ?? [])
        : edit.snippets
          ? rangesOf(text, edit.snippets)
          : [{ start: 0, end: text.length }];
      // A removal beside an added line is a replacement: the added line
      // places it, and its gap (above an indented header) would name the encloser.
      const added = new Set(edit.lines ?? []);
      const removed = (edit.removed ?? []).filter((line) => !added.has(line) && !added.has(line - 1));
      const gaps = byLine ? gapsOfLines(text, removed) : [];
      if (ranges.length === 0 && gaps.length === 0) continue;
      const reach = touched(text, scan, ranges, gaps);
      // A name declared twice in this file (a handler of one name in two
      // components) cannot say which one the edit reached.
      const declared = new Map<string, number>();
      for (const span of scan.spans) declared.set(span.name, (declared.get(span.name) ?? 0) + 1);
      const names = reach.names.filter((name) => declared.get(name) === 1);
      isolated("op", () => {
        const rows: FactRow[] = [];
        for (const name of names) {
          const own = namedRows(facts, at.repo, name, at.rel);
          if (!holdsSeveral(own)) {
            rows.push(...own);
            continue;
          }
          // Several inline handlers or calls under one name: each row by its own site.
          const span = scan.spans.find((one) => one.name === name)!;
          rows.push(...unnamedRows(own, reach.reachedIn(span), scan.calls));
        }
        if (reach.calls.length > 0) {
          const unnamed = facts.unnamed(at.repo, at.rel);
          if (unnamed.length > 0) rows.push(...unnamedRows(unnamed, reach.calls, scan.calls));
        }
        return rows.map((row) => ({ key: rowKey(row), text: row.text, named: pathsOf(row.others) }));
      });
      // The functions' callers in other files: only the row in this file.
      isolated("fn", () =>
        names.flatMap((name) =>
          facts
            .callers(at.repo, name)
            .filter((fn) => fn.file === at.rel)
            .map((fn) => ({ key: `fn::${fn.repo}::${fn.name}`, text: fn.text, named: pathsOf(fn.others) })),
        ),
      );
      isolated("name", () => sameNameLines(facts, at, where, text, scan, edit, isJsx(edit.file)));
    }
    // At most `MAX_FACT_LINES`, kind by kind, each line once per session.
    for (const kind of KINDS) {
      for (const candidate of found[kind]) {
        if (lines.length >= MAX_FACT_LINES) break;
        if (said.has(candidate.key) || candidate.key in claims) continue;
        lines.push(candidate.text);
        claims[candidate.key] = { at: now, named: candidate.named };
      }
    }
  } catch (error) {
    log("fact lines failed", String(error));
  } finally {
    store?.close();
  }
  return done();
}
