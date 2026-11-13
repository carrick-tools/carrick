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
// calls `markFacts` after printing). An op or fn line prints only while the
// peer files it names read as at the index commit (`peers.ts`). Never throws.

import fs from "node:fs";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { createLogger } from "../log.ts";
import { factsSaid } from "../hook/reuse.ts";
import { commonGitDir, linkedCheckoutTop } from "../hook/worktree.ts";
import { gapsOfLines, rangesOf, rangesOfLines, touched } from "./match.ts";
import type { CallSpan } from "./match.ts";
import { factsEnabled } from "./enabled.ts";
import { FACTS_STALE_MS, openFacts } from "./store.ts";
import type { ContractRow, FactRow, FactStore, LocalRepo } from "./store.ts";
import { fileMemo, isJsx, SCANNED } from "./files.ts";
import { commitsOf, indexedCommit, localCheckouts, peersUnchanged } from "./peers.ts";
import type { PeerContext } from "./peers.ts";
import { sameNameLines } from "./rules/same-name.ts";
import { isManifest, packageLines } from "./rules/packages.ts";

const log = createLogger("carrick-facts");

/** At most this many fact lines for one action. */
export const MAX_FACT_LINES = 3;

/** The name kind stops after this many lines whose other file failed its check (R33.5). */
const MAX_NAME_FAILURES = 3;

/** Op and fn lines stop after this many failed peer proofs in one action (R36.3). */
const MAX_PROOF_FAILURES = 3;

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

export type FactOutcome = {
  lines: string[];
  /** The row keys of `lines`, in order: a second call in the same action passes them as `options.exclude`. */
  used: string[];
  /** Row key -> delivery stamp and the absolute paths the line pointed at; claim with `markFacts` once printed. */
  claims: Record<string, { at: number; named: string[] }>;
  /** Time spent opening the fact file and matching. */
  ms: number;
  /** The fact file is missing, unreadable or older than `FACTS_STALE_MS`: start a background refresh. */
  stale: boolean;
  /**
   * The edited files (as given) whose callers the fact lines answered: an fn
   * line for the file printed in this action, or was said earlier in this
   * session. Only for these is the probe's uses-count line left out, at edit
   * time and in a late answer (carrick#4017; plan section 33, R33.6).
   */
  callersRead: string[];
  /**
   * The fact file opened for this root: there, readable, this project's.
   * With it the edit hook asks no probe and queues the edit (T12-2).
   */
  present: boolean;
};

/**
 * A line one rule found, before the cap: its once-per-session key and the
 * paths it points at. `holds`, when set, is a costlier check run only for a
 * line about to print within the cap; false drops the line.
 */
export type Candidate = {
  key: string;
  text: string;
  named: string[];
  holds?: () => boolean;
  /** An fn line's edited file (as given), for `callersRead`. */
  file?: string;
  /** Keys claimed with this line when it prints, so another kind does not say the same thing again. */
  alsoClaims?: string[];
};

/**
 * The row kinds in the order their lines print, before the cap of
 * `MAX_FACT_LINES`. The `contract` kind (carrick-cloud `edit-facts-wire.md`,
 * `k: "contract"`) has candidates only when `factLines` is asked for it
 * (`options.contract`); with none, the order is op, fn, name, pkg as ever.
 */
const KINDS = ["contract", "op", "fn", "name", "pkg"] as const;
type Kind = (typeof KINDS)[number];

/** The kinds `pickLines` always takes; `contract` is optional. */
type Found = Record<Exclude<Kind, "contract">, readonly Candidate[]> & { contract?: readonly Candidate[] };

/** The main checkout a linked worktree belongs to, from its `.git` file. */
function mainCheckoutOf(worktreeTop: string): string | null {
  const common = commonGitDir(worktreeTop);
  return common ? path.dirname(common) : null;
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
function methodOf(row: Pick<FactRow, "protocol" | "label">): string | null {
  return row.protocol === "http" ? (row.label.split(" ")[0] ?? "").toLowerCase() : null;
}

/**
 * Whether `call` is written as `row`'s site: the site as a direct string
 * argument, and an HTTP row's method as the callee's name. Any other
 * protocol's producer (a listener, subscriber, resolver) also needs a
 * function written in the call; its consumer (an emit, a publish) never
 * fits, as the row does not say the callee's name.
 */
function callFits(call: CallSpan, row: Pick<FactRow, "site" | "side">, method: string | null): boolean {
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
function holdsSeveral(rows: readonly Pick<FactRow, "side" | "protocol">[]): boolean {
  const seen = new Set<string>();
  for (const row of rows) {
    const group = `${row.side} ${row.protocol}`;
    if (seen.has(group)) return true;
    seen.add(group);
  }
  return false;
}

function rowKey(row: Pick<FactRow, "repo" | "name" | "file" | "side" | "label">): string {
  return `${row.repo}::${row.name ?? row.file}::${row.side}::${row.label}`;
}

/** The most groups of one contract row that print: its first two (plan: at most 2 lines per operation). */
const CONTRACT_GROUPS_PRINTED = 2;

/** Whether `row` is one the laptop may print: a response row whose first group carries its wording. */
function printable(row: ContractRow): boolean {
  return row.direction === "response" && typeof row.groups[0]?.lead === "string";
}

/** A contract row's place in the file, for matching it to the op rows an edit reached. */
function contractId(row: Pick<ContractRow, "side" | "label" | "file" | "line">): string {
  return `${row.side}\0${row.label}\0${row.file}\0${row.line}`;
}

/** Contract rows in the order their lines are tried within a tier: by line, then label. */
function byLineThenLabel(rows: ContractRow[]): ContractRow[] {
  return [...rows].sort((a, b) => (a.line ?? Infinity) - (b.line ?? Infinity) || (a.label < b.label ? -1 : a.label > b.label ? 1 : 0));
}

/** Whether a call named `member` is a call to one of `names` (`Class.method` is called by its last part). */
function callsName(member: string | null, names: readonly string[]): boolean {
  return member !== null && names.some((name) => name === member || name.endsWith(`.${member}`));
}

/**
 * Whether the code that writes `row` calls one of `names`: its named span when
 * that name is declared once in the file and holds one row of its kind, else
 * the one call that writes its `site` (`callFits`). A value reference is not a call.
 */
function bodyCalls(row: ContractRow, fileRows: readonly ContractRow[], names: readonly string[], scan: { spans: readonly { name: string; start: number; end: number }[]; calls: readonly CallSpan[] }): boolean {
  const own = row.name === null ? [] : scan.spans.filter((span) => span.name === row.name);
  const sameName = row.name === null ? [] : fileRows.filter((other) => other.name === row.name);
  let body: { start: number; end: number; self: CallSpan | null } | null = null;
  if (own.length === 1 && !holdsSeveral(sameName)) {
    body = { start: own[0]!.start, end: own[0]!.end, self: null };
  } else if (row.site !== null) {
    const method = methodOf(row);
    if (fileRows.filter((other) => other.site === row.site && methodOf(other) === method).length === 1) {
      const holding = scan.calls.filter((call) => callFits(call, row, method));
      if (holding.length === 1) body = { start: holding[0]!.start, end: holding[0]!.end, self: holding[0]! };
    }
  }
  if (body === null) return false;
  const { start, end, self } = body;
  return scan.calls.some((call) => call !== self && call.start >= start && call.end <= end && callsName(call.member, names) && call.member !== row.name);
}

/** A candidate's check, isolated like the lookups (R36.6): one that throws is a failed check, the line silent. */
function held(kind: Kind, candidate: Candidate): boolean {
  try {
    return candidate.holds ? candidate.holds() : true;
  } catch (error) {
    log(`${kind} fact check failed`, String(error));
    return false;
  }
}

/**
 * The lines to print from `found`: at most `limit` (default `MAX_FACT_LINES`),
 * kind by kind and, inside a kind, in the order the lookups found them (T2,
 * P1), each once per session. A key in `said` or `options.exclude` (printed
 * earlier in this action by another call) is skipped. A line past the limit
 * is not claimed and is not mentioned. The name kind stops after
 * `MAX_NAME_FAILURES` failed checks (R33.5): each one may scan a whole other
 * file. Op and fn share a bound of `MAX_PROOF_FAILURES` failed peer proofs
 * (R36.3): each may run git. Only a failed check counts, never a line
 * skipped as said, excluded or capped. Each printed line is added to `claims`.
 * `options.limit` is never above `MAX_FACT_LINES`; an absent or non-finite
 * limit (NaN, Infinity) means `MAX_FACT_LINES`, and 0 or less prints nothing.
 */
export function pickLines(
  found: Found,
  said: ReadonlySet<string>,
  claims: FactOutcome["claims"],
  now: number,
  options: { limit?: number; exclude?: ReadonlySet<string> } = {},
): { lines: string[]; used: string[] } {
  const limit = Number.isFinite(options.limit) ? Math.min(options.limit!, MAX_FACT_LINES) : MAX_FACT_LINES;
  const lines: string[] = [];
  const used: string[] = [];
  let proofFailures = 0;
  for (const kind of KINDS) {
    let failed = 0;
    const proved = kind === "contract" || kind === "op" || kind === "fn";
    for (const candidate of found[kind] ?? []) {
      if (lines.length >= limit) break;
      if (kind === "name" && failed >= MAX_NAME_FAILURES) break;
      if (proved && proofFailures >= MAX_PROOF_FAILURES) break;
      if (said.has(candidate.key) || candidate.key in claims || options.exclude?.has(candidate.key)) continue;
      if (!held(kind, candidate)) {
        failed += 1;
        if (proved) proofFailures += 1;
        continue;
      }
      lines.push(candidate.text);
      used.push(candidate.key);
      claims[candidate.key] = { at: now, named: candidate.named };
      // A contract line says what the operation's op line says: that one is claimed with it.
      for (const also of candidate.alsoClaims ?? []) claims[also] ??= { at: now, named: candidate.named };
    }
  }
  return { lines, used };
}

/**
 * Open the fact file and the lines for `edits`. `options.limit` and
 * `options.exclude` (keys an earlier call in the same action printed, from
 * its `used`) let several calls share one cap. `options.contract` (default
 * false, the caller's switch: `contractLinesEnabled`) adds the contract kind:
 * for a file that holds contract rows, the line naming the other end of an
 * operation and its type, tiered by how the edit reaches the operation
 * (tier 1: the op matcher; tier 2: a changed function the operation's code
 * calls; tier 3: the file's other operations, on its first contract line of
 * the session). Reads never pass it.
 */
export async function factLines(
  root: string,
  edits: readonly FactEdit[],
  session: string | undefined,
  options: { home?: string; now?: number; env?: NodeJS.ProcessEnv; limit?: number; exclude?: ReadonlySet<string>; contract?: boolean } = {},
): Promise<FactOutcome> {
  const started = performance.now();
  const now = options.now ?? Date.now();
  const lines: string[] = [];
  const used: string[] = [];
  const claims: FactOutcome["claims"] = {};
  const callersRead: string[] = [];
  let stale = true;
  let present = false;
  const done = (): FactOutcome => ({ lines, used, claims, stale, callersRead, present, ms: Math.round((performance.now() - started) * 10) / 10 });
  let store: FactStore | null = null;
  const found: Record<Exclude<Kind, "contract">, Candidate[]> = { op: [], fn: [], name: [], pkg: [] };
  // The contract kind, by tier, joined after the loop: every tier-1 line before any tier 2, then tier 3.
  const tiers: Candidate[][] = [[], [], []];
  const taken = new Set<ContractRow>();
  const rest: Array<{ at: NonNullable<ReturnType<typeof locate>>; rows: ContractRow[] }> = [];
  // Off by default (`enabled.ts`): the store is not opened and nothing refreshes.
  if (!factsEnabled(options.env)) {
    stale = false;
    return done();
  }
  try {
    store = await openFacts(root);
    if (store === null) return done();
    present = true;
    const facts: FactStore = store;
    const fetched = Date.parse(store.meta["fetched_at"] ?? "");
    stale = !Number.isFinite(fetched) || now - fetched > FACTS_STALE_MS;
    const said = session ? factsSaid(session, options.home) : new Set<string>();
    const where = new Map(store.repos.map((entry) => [entry.repo, entry.path]));
    // One memo of file reads and scans for the whole call, shared by every rule.
    const memo = fileMemo();
    const commits = commitsOf(store.meta);
    const proofs: PeerContext["proofs"] = new Map();
    const missing: PeerContext["missing"] = new Set();
    // A peer-proof git call that ran out of time, shared across the call's edited files.
    const git = { failed: false };
    // Where each repo is checked out here, for every kind (R40.2).
    const checkouts = localCheckouts(where);
    const peersHold = (at: { repo: string; top: string }, others: ReadonlyArray<{ repo: string; file: string }>) => (): boolean => {
      const ctx: PeerContext = { at, checkouts, commits, memo, env: options.env, proofs, missing, gitFailed: git.failed };
      const answer = peersUnchanged(others, ctx);
      git.failed = ctx.gitFailed;
      return answer;
    };
    const pathsOf = (others: ReadonlyArray<{ repo: string; file: string }>): string[] =>
      others.flatMap((other) => {
        const base = where.get(other.repo);
        return base ? [path.join(base, other.file)] : [];
      });
    // Each kind's lookup on its own: a row of one kind that throws (a field
    // ingest let through) loses only that kind's lines for the file.
    const isolated = (kind: Exclude<Kind, "contract">, lookup: () => Candidate[]): void => {
      try {
        found[kind].push(...lookup());
      } catch (error) {
        log(`${kind} fact lines failed`, String(error));
      }
    };
    // The contract candidates of one row: its first two groups, each a line.
    // A place on disk must read as at the index commit (`holds`); one off disk
    // is said with the note, and only when the file holds its repo's full commit.
    const contractCandidates = (at: { repo: string; top: string }, row: ContractRow): Candidate[] =>
      row.groups.slice(0, CONTRACT_GROUPS_PRINTED).flatMap((group, index) => {
        if (typeof group.lead !== "string" || typeof group.more !== "string" || typeof group.tail !== "string") return [];
        const shown = group.places.slice(0, group.shown);
        if (shown.length === 0) return [];
        const onDisk: Array<{ repo: string; file: string }> = [];
        const parts: string[] = [];
        for (const place of shown) {
          if (checkouts.place(place.repo, at) !== null) {
            onDisk.push(place);
            parts.push(place.loc);
            continue;
          }
          const commit = indexedCommit(commits, place.repo);
          if (commit === null) return [];
          parts.push(`${place.loc} (another repo, indexed at ${commit.slice(0, 8)}, not checked out here)`);
        }
        return [
          {
            key: `contract::${row.repo}::${row.file}::${row.side}::${row.label}::${index}`,
            text: `${group.lead}${parts.join(", ")}${group.more}${group.tail}`,
            named: pathsOf(onDisk),
            holds: peersHold(at, onDisk),
            alsoClaims: [rowKey(row)],
          },
        ];
      });
    for (const edit of edits) {
      const at = locate(edit.file, store.repos);
      if (at === null) continue;
      if (isManifest(edit.file)) {
        const manifest = memo.text(edit.file);
        if (manifest !== null) isolated("pkg", () => packageLines(facts, at, checkouts, manifest, edit, memo));
        continue;
      }
      if (!SCANNED.test(edit.file)) continue;
      // The file's contract rows. Only the file's first contract line of the
      // session may add the rest of its operations (tier 3), which needs no
      // changed lines and no clean scan.
      let contractRows: ContractRow[] = [];
      if (options.contract === true) {
        try {
          contractRows = facts.contracts(at.repo, at.rel).filter(printable);
        } catch (error) {
          log("contract fact lines failed", String(error));
        }
        const prefix = `contract::${at.repo}::${at.rel}::`;
        const earlier = (key: string) => key.startsWith(prefix);
        const first = ![...said].some(earlier) && !(options.exclude && [...options.exclude].some(earlier));
        if (contractRows.length > 0 && first) rest.push({ at, rows: contractRows });
      }
      const byLine = edit.lines || edit.removed;
      // Changed lines not known (a Write over a file, a shell write, a patch
      // hunk that cannot be placed): nothing is said.
      if (!byLine && !edit.snippets && !edit.whole) continue;
      // A file over `MAX_FACT_FILE_BYTES` is generated or bundled: never read
      // in a hook's time. One that does not scan cleanly says nothing.
      const read = memo.scanned(edit.file);
      if (read === null) {
        log(`${at.rel} was not read cleanly; no fact lines for it`);
        continue;
      }
      const { text, scan } = read;
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
      // The op rows this edit reaches: tier 1 of the contract kind.
      const reachedOps: FactRow[] = [];
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
        reachedOps.push(...rows);
        return rows.map((row) => ({ key: rowKey(row), text: row.text, named: pathsOf(row.others), holds: peersHold(at, row.others) }));
      });
      if (contractRows.length > 0) {
        try {
          const reached = new Set(reachedOps.map(contractId));
          const one = contractRows.filter((row) => reached.has(contractId(row)));
          for (const row of one) taken.add(row);
          const two = contractRows.filter((row) => !taken.has(row) && bodyCalls(row, contractRows, names, scan));
          for (const row of two) taken.add(row);
          tiers[0]!.push(...byLineThenLabel(one).flatMap((row) => contractCandidates(at, row)));
          tiers[1]!.push(...byLineThenLabel(two).flatMap((row) => contractCandidates(at, row)));
        } catch (error) {
          log("contract fact lines failed", String(error));
        }
      }
      // The functions' callers in other files: only the row in this file.
      isolated("fn", () =>
        names.flatMap((name) =>
          facts
            .callers(at.repo, name)
            .filter((fn) => fn.file === at.rel)
            .map((fn) => ({
              key: `fn::${fn.repo}::${fn.file}::${fn.name}`,
              text: fn.text,
              named: pathsOf(fn.others),
              holds: peersHold(at, fn.others),
              file: edit.file,
            })),
        ),
      );
      isolated("name", () => sameNameLines(facts, at, checkouts, text, scan, edit, isJsx(edit.file), memo));
    }
    for (const one of rest) {
      try {
        tiers[2]!.push(...byLineThenLabel(one.rows.filter((row) => !taken.has(row))).flatMap((row) => contractCandidates(one.at, row)));
      } catch (error) {
        log("contract fact lines failed", String(error));
      }
    }
    const picked = pickLines({ ...found, contract: tiers.flat() }, said, claims, now, { limit: options.limit, exclude: options.exclude });
    lines.push(...picked.lines);
    used.push(...picked.used);
    // A file's callers are answered only where its fn line printed now or
    // was said earlier in this session (R33.6).
    for (const candidate of found.fn) {
      if (candidate.file === undefined || callersRead.includes(candidate.file)) continue;
      if (said.has(candidate.key) || candidate.key in claims) callersRead.push(candidate.file);
    }
  } catch (error) {
    log("fact lines failed", String(error));
  } finally {
    store?.close();
  }
  return done();
}
