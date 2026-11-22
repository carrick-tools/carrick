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
import type { CallSpan, Range } from "./match.ts";
import { factsEnabled } from "./enabled.ts";
import { FACTS_STALE_MS, openFacts } from "./store.ts";
import type { ContractGroup, ContractRow, FactRow, FactStore, LocalRepo } from "./store.ts";
import { fileMemo, isJsx, SCANNED } from "./files.ts";
import { commitsOf, indexedCommit, localCheckouts, peersUnchanged } from "./peers.ts";
import type { PeerContext, PeerStop } from "./peers.ts";
import { sameNameLines } from "./rules/same-name.ts";
import { isManifest, packageLines } from "./rules/packages.ts";

const log = createLogger("carrick-facts");

/** At most this many fact lines for one action. */
export const MAX_FACT_LINES = 3;

/** The name kind stops after this many lines whose other file failed its check (R33.5). */
const MAX_NAME_FAILURES = 3;

/** The most `matched` lines one edit prints (SD-D8). */
export const MAX_MATCHED_LINES = 2;

/** Whether a changed range or gap of `text` (character offsets) sits on the 1-based `line`. */
function touchesLine(text: string, line: number, ranges: readonly Range[], gaps: readonly number[]): boolean {
  const [own] = rangesOfLines(text, [line]);
  if (own === undefined) return false;
  return ranges.some((range) => range.start <= own.end && range.end > own.start) || gaps.some((gap) => gap >= own.start && gap <= own.end);
}

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
 * the edit can be rebuilt. `read` is a Read of the file (T5): the whole file
 * is reached and only the op and fn lookups run; never `whole`, which would
 * make every declaration new for the same-name rule.
 */
export type FactEdit = {
  file: string;
  read?: boolean;
  lines?: number[] | null;
  removed?: number[] | null;
  removedText?: string[] | null;
  snippets?: string[] | null;
  whole?: boolean;
};

/**
 * Why a fact-line candidate was dropped and nothing was said (carrick#4052).
 * Protocol-blind: every kind of line counts the same words. Peer words are
 * `PeerStop`; the rest are `no_changed_lines` (the changed lines of a
 * file are not known), `not_read_cleanly` (the file does not scan cleanly) and
 * `rule_failed` (a check or lookup threw).
 */
export type StopWord = PeerStop | "no_changed_lines" | "not_read_cleanly" | "rule_failed";

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
  /**
   * The `read` files (as given) the fact file answered: the store holds fn
   * rows, the file sits in a mapped repo, scanned cleanly, both lookups ran
   * without throwing, and the file declares a named thing or holds an unnamed
   * op row. Only for these does a Read skip the binary's `check`; when
   * answered and nothing prints, nothing prints (T5, P7).
   */
  answered: string[];
  /**
   * Stop word -> how many candidates it dropped in this call, once per kind
   * and row, never once per peer file. Written to the timing row as `facts_stops`.
   */
  stops: Record<string, number>;
};

/**
 * A line one rule found, before the cap: its once-per-session key and the
 * paths it points at. `holds`, when set, is a costlier check run only for a
 * line about to print within the cap; null holds, a stop word drops the line.
 */
export type Candidate = {
  key: string;
  text: string;
  named: string[];
  holds?: () => StopWord | null;
  /** An fn line's edited file (as given), for `callersRead`. */
  file?: string;
  /** Keys claimed with this line when it prints, so another kind does not say the same thing again. */
  alsoClaims?: string[];
  /**
   * Keys that, once said, claimed or excluded, make this line a repeat: it is
   * skipped as a said line is, before its check (a chain line whose route's
   * contract line another line in this action or session already said).
   */
  skipIf?: string[];
};

/**
 * The row kinds in the order their lines print, before the cap of
 * `MAX_FACT_LINES`. The `contract` kind (carrick-cloud `edit-facts-wire.md`,
 * `k: "contract"`) has candidates only when `factLines` is asked for it
 * (`options.contract`); with none, the order is op, fn, name, pkg as ever.
 * `matched` (a pair a model found) prints after `op`, at most `MAX_MATCHED_LINES`.
 */
const KINDS = ["contract", "op", "matched", "fn", "name", "pkg"] as const;
type Kind = (typeof KINDS)[number];

/** The kinds `pickLines` always takes; `contract` and `matched` are optional. */
type Found = Record<Exclude<Kind, "contract" | "matched">, readonly Candidate[]> & { contract?: readonly Candidate[]; matched?: readonly Candidate[] };

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

/** A request row prints one group: its line follows every response line, so it stays short. */
const CONTRACT_REQUEST_GROUPS_PRINTED = 1;

/** The routes beyond the one a chain line names, as a count only (the Edit line and the Read line say it the same way): "" for a function with one route. */
function otherRoutes(total: number): string {
  return total > 1 ? ` and ${total - 1} other route${total === 2 ? "" : "s"}` : "";
}

/** The most callers a chain line names; the rest are counted. */
const CHAIN_CALLERS_PRINTED = 3;

/** Whether `row` is one the laptop may print: its first group carries its wording (the cloud decides which rows are worded). */
function printable(row: ContractRow): boolean {
  return typeof row.groups[0]?.lead === "string";
}

/** A contract row's place in the file, for matching it to the op rows an edit reached. */
function contractId(row: Pick<ContractRow, "side" | "label" | "file" | "line">): string {
  return `${row.side}\0${row.label}\0${row.file}\0${row.line}`;
}

/** A contract row's place with its repo: the same row whichever read of the store returned it. */
function takenId(row: Pick<ContractRow, "repo" | "side" | "label" | "file" | "line">): string {
  return `${row.repo}\0${contractId(row)}`;
}

/** The once-per-session key of a contract row's group `index`. */
export function contractKey(row: Pick<ContractRow, "repo" | "file" | "side" | "label"> & { direction?: ContractRow["direction"] }, index: number): string {
  const direction = row.direction === "request" ? "::request" : "";
  return `contract::${row.repo}::${row.file}::${row.side}::${row.label}${direction}::${index}`;
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
function held(kind: Kind, candidate: Candidate): StopWord | null {
  try {
    return candidate.holds ? candidate.holds() : null;
  } catch (error) {
    log(`${kind} fact check failed`, String(error));
    return "rule_failed";
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
 * `options.stops`, when given, gets a count per stop word of the candidates dropped. `options.limit` is never above `MAX_FACT_LINES`; an absent or non-finite
 * limit (NaN, Infinity) means `MAX_FACT_LINES`, and 0 or less prints nothing.
 */
export function pickLines(
  found: Found,
  said: ReadonlySet<string>,
  claims: FactOutcome["claims"],
  now: number,
  options: { limit?: number; exclude?: ReadonlySet<string>; stops?: Record<string, number> } = {},
): { lines: string[]; used: string[] } {
  const limit = Number.isFinite(options.limit) ? Math.min(options.limit!, MAX_FACT_LINES) : MAX_FACT_LINES;
  const lines: string[] = [];
  const used: string[] = [];
  let proofFailures = 0;
  // A dropped candidate counts once per kind and row, not once per peer file.
  const counted = new Set<string>();
  for (const kind of KINDS) {
    let failed = 0;
    const proved = kind === "contract" || kind === "op" || kind === "fn";
    for (const candidate of found[kind] ?? []) {
      if (lines.length >= limit) break;
      if (kind === "name" && failed >= MAX_NAME_FAILURES) break;
      if (proved && proofFailures >= MAX_PROOF_FAILURES) break;
      const repeat = (key: string) => said.has(key) || key in claims || options.exclude?.has(key) === true;
      if (repeat(candidate.key) || candidate.skipIf?.some(repeat)) continue;
      const stop = held(kind, candidate);
      if (stop !== null) {
        if (options.stops && !counted.has(`${kind}::${candidate.key}`)) {
          counted.add(`${kind}::${candidate.key}`);
          options.stops[stop] = (options.stops[stop] ?? 0) + 1;
        }
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
 * the session). `options.chain` (default false, the caller's switch:
 * `chainLinesEnabled`; read only with `options.contract`) adds the chain
 * line between tiers 2 and 3: for a changed function the cloud walked to from
 * routes' handlers (`k: "chain"`), one line naming the first of its routes
 * whose line holds here, in the route's lead and its contract row's first
 * group. A `read` edit never gets the contract kind; with `options.chain` it gets the chain kind alone, one line for the file (`chainCandidates`), and with `options.chainOnly` (the Read switch is off) nothing else, and the file is not `answered`.
 */
export async function factLines(
  root: string,
  edits: readonly FactEdit[],
  session: string | undefined,
  options: { home?: string; now?: number; env?: NodeJS.ProcessEnv; limit?: number; exclude?: ReadonlySet<string>; contract?: boolean; chain?: boolean; chainOnly?: boolean; store?: FactStore | null } = {},
): Promise<FactOutcome> {
  const started = performance.now();
  const now = options.now ?? Date.now();
  const lines: string[] = [];
  const used: string[] = [];
  const claims: FactOutcome["claims"] = {};
  const callersRead: string[] = [];
  let stale = true;
  let present = false;
  const answered: string[] = [];
  const stops: Record<string, number> = {};
  const stopped = (word: StopWord): void => {
    stops[word] = (stops[word] ?? 0) + 1;
  };
  const done = (): FactOutcome => ({ lines, used, claims, stale, callersRead, present, answered, stops, ms: Math.round((performance.now() - started) * 10) / 10 });
  let store: FactStore | null = null;
  const found: Record<Exclude<Kind, "contract">, Candidate[]> = { op: [], matched: [], fn: [], name: [], pkg: [] };
  // The contract kind, by tier, joined after the loop: every tier-1 line
  // before any tier 2, then the chain lines, then tier 3.
  const tiers: Candidate[][] = [[], [], [], [], []];
  const taken = new Set<ContractRow>();
  // The same rows by repo and place, for the chain lines, which read their contract rows afresh.
  const takenIds = new Set<string>();
  const rest: Array<{ at: NonNullable<ReturnType<typeof locate>>; rows: ContractRow[] }> = [];
  // The changed functions of each edited file whose chain rows are looked up after the loop, when every file's tiers 1-2 are known.
  const chained: Array<{ at: NonNullable<ReturnType<typeof locate>>; names: string[]; outer: Map<string, string[]>; read: boolean }> = [];
  // Off by default (`enabled.ts`): the store is not opened and nothing refreshes.
  if (!factsEnabled(options.env)) {
    stale = false;
    options.store?.close();
    return done();
  }
  try {
    // A caller that already opened the store hands it in; this call closes it.
    store = options.store !== undefined ? options.store : await openFacts(root);
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
    const peersHold = (at: { repo: string; top: string }, others: ReadonlyArray<{ repo: string; file: string }>) => (): StopWord | null => {
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
    // Returns whether the lookup ran without throwing.
    const isolated = (kind: Exclude<Kind, "contract">, lookup: () => Candidate[]): boolean => {
      try {
        found[kind].push(...lookup());
        return true;
      } catch (error) {
        log(`${kind} fact lines failed`, String(error));
        stopped("rule_failed");
        return false;
      }
    };
    // A contract group's words after its lead: the shown places, `more` and
    // `tail`, and the places on disk the line names. A place on disk must read
    // as at the index commit (the caller's `holds`); one off disk is said with
    // the note, and only when the file holds its repo's full commit (else null).
    const groupWords = (at: { repo: string; top: string }, group: ContractGroup): { words: string; onDisk: Array<{ repo: string; file: string }> } | null => {
      if (typeof group.lead !== "string" || typeof group.more !== "string" || typeof group.tail !== "string") return null;
      const shown = group.places.slice(0, group.shown);
      if (shown.length === 0) return null;
      const onDisk: Array<{ repo: string; file: string }> = [];
      const parts: string[] = [];
      for (const place of shown) {
        if (checkouts.place(place.repo, at) !== null) {
          onDisk.push(place);
          parts.push(place.loc);
          continue;
        }
        const commit = indexedCommit(commits, place.repo);
        if (commit === null) return null;
        parts.push(`${place.loc} (another repo, indexed at ${commit.slice(0, 8)}, not checked out here)`);
      }
      return { words: `${parts.join(", ")}${group.more}${group.tail}`, onDisk };
    };
    // The callers of a route in services other than its own, from its
    // contract rows (response, then request): typed places, then untyped ones.
    // A place a response group holds keeps that group's `tail` (", which
    // expects: <type>", worded and shortened by the cloud: the one way the
    // type is composed); any other place has none.
    const callerPlaces = (rows: readonly ContractRow[], service: string): Array<{ repo: string; loc: string; tail: string }> => {
      const out = new Map<string, { repo: string; loc: string; tail: string }>();
      const add = (repo: string, owner: string, file: string, line: number | null, tail: string, loc?: string) => {
        if (owner === service) return;
        const at = loc ?? (line === null ? `${repo}/${file}` : `${repo}/${file}:${line}`);
        if (!out.has(at) || (out.get(at)!.tail === "" && tail !== "")) out.set(at, { repo, loc: at, tail });
      };
      const ordered = [...rows].sort((a, b) => (a.direction === b.direction ? 0 : a.direction === "response" ? -1 : 1));
      for (const row of ordered) {
        for (const group of row.groups) {
          const tail = row.direction === "response" && typeof group.tail === "string" ? group.tail : "";
          for (const one of group.places.slice(0, group.shown)) add(one.repo, one.service, one.file, one.line, tail, one.loc);
        }
      }
      for (const row of ordered) for (const one of row.untyped) add(one.repo, one.service, one.file, one.line, "");
      return [...out.values()];
    };
    // `places` as the line words them: a place on disk by its place, one off
    // disk with the note (null when its repo has no full commit in the file),
    // each followed by its type words; callers are split by ";" when any has them.
    const callerWords = (at: { repo: string; top: string }, places: ReadonlyArray<{ repo: string; loc: string; tail: string }>): { words: string; onDisk: Array<{ repo: string; file: string }> } | null => {
      const onDisk: Array<{ repo: string; file: string }> = [];
      const parts: string[] = [];
      for (const place of places) {
        const local = checkouts.place(place.repo, at);
        if (local !== null) {
          onDisk.push({ repo: place.repo, file: place.loc.slice(place.repo.length + 1).replace(/:\d+$/, "") });
          parts.push(`${place.loc}${place.tail}`);
          continue;
        }
        const commit = indexedCommit(commits, place.repo);
        if (commit === null) return null;
        parts.push(`${place.loc} (another repo, indexed at ${commit.slice(0, 8)}, not checked out here)${place.tail}`);
      }
      return { words: parts.join(places.some((place) => place.tail !== "") ? "; " : ", "), onDisk };
    };
    // The contract candidates of one row: its first two groups (a request row: its first), each a line.
    const contractCandidates = (at: { repo: string; top: string }, row: ContractRow): Candidate[] =>
      row.groups.slice(0, row.direction === "request" ? CONTRACT_REQUEST_GROUPS_PRINTED : CONTRACT_GROUPS_PRINTED).flatMap((group, index) => {
        const composed = groupWords(at, group);
        if (composed === null) return [];
        return [
          {
            key: contractKey(row, index),
            text: `${group.lead}${composed.words}`,
            named: pathsOf(composed.onDisk),
            holds: peersHold(at, composed.onDisk),
            alsoClaims: [rowKey(row)],
          },
        ];
      });
    // The chain candidates of the changed functions `names` of the edited
    // file at `at`: per chain row, one candidate per kept route, all under
    // the row's one key, so `pickLines` prints the first that holds and the
    // rest fall silent. A name with no chain row looks on the next named
    // thing out (`outer`, innermost first). A route is skipped when neither
    // its response nor its request contract row is printable, the response
    // row was taken by tier 1 or 2 in this action, or its group 0 was said
    // (`skipIf`). The line names the callers in other services from both
    // rows (typed places and untyped ones: fact rows only, the cloud drops a
    // model's reading), the route's hop files and its registration file
    // (other than the edited file): each place on disk must read as at the
    // index commit.
    // On a Read (`read`) the same lines are found for every named thing in the
    // file, but only the best one is said: fewest hops, then most callers in
    // other services, then the order found. A route the read file registers is
    // skipped (its own op line says it).
    const chainCandidates = (at: { repo: string; rel: string; top: string }, names: readonly string[], outer: ReadonlyMap<string, readonly string[]>, read = false): Candidate[] => {
      const ranked: Array<{ candidate: Candidate; depth: number; callers: number }> = [];
      for (const name of names) {
        let rows = facts.chains(at.repo, at.rel, name);
        for (const encloser of outer.get(name) ?? []) {
          if (rows.length > 0) break;
          rows = facts.chains(at.repo, at.rel, encloser);
        }
        for (const chain of rows) {
          for (const route of chain.routes) {
            if (read && route.repo === at.repo && route.file === at.rel) continue;
            const same = facts.contracts(route.repo, route.file).filter((one) => one.side === route.side && one.label === route.label && one.line === route.line && printable(one));
            const response = same.find((one) => one.direction === "response");
            const base = response ?? same[0];
            if (base === undefined || (response !== undefined && takenIds.has(takenId(response)))) continue;
            const callers = callerPlaces(same, route.service);
            if (callers.length === 0) continue;
            const words = callerWords(at, callers.slice(0, CHAIN_CALLERS_PRINTED));
            if (words === null) continue;
            const peers = [...new Set([...route.path.map((hop) => hop.file), route.file])]
              .filter((file) => !(route.repo === at.repo && file === at.rel))
              .map((file) => ({ repo: route.repo, file }));
            const others = [...peers, ...words.onDisk];
            const groupKey = contractKey(base, 0);
            const hop = route.path[0]!;
            const hopAt = hop.call === null ? hop.file : `${hop.file}:${hop.call}`;
            const where = route.depth === 1 ? `(${hopAt})` : `, through ${hop.name} (${hopAt})`;
            const rest = callers.length > CHAIN_CALLERS_PRINTED ? ` and ${callers.length - CHAIN_CALLERS_PRINTED} more` : "";
            ranked.push({
              depth: route.depth,
              callers: callers.length,
              candidate: {
                key: `chain::${chain.repo}::${chain.file}::${chain.name}`,
                text: `Carrick: ${chain.name} (${chain.file}:${chain.line}) runs in the handler of ${route.label}${route.depth === 1 ? " " : ""}${where}${otherRoutes(chain.total)}. Other services call that route: ${words.words}${rest}. If your change adds a side effect or changes what the route returns, open those callers first.`,
                named: pathsOf(others),
                holds: peersHold(at, others),
                skipIf: [groupKey],
                alsoClaims: [groupKey, ...(response !== undefined ? [rowKey(response)] : [])],
              },
            });
          }
        }
      }
      if (!read) return ranked.map((one) => one.candidate);
      // Array.prototype.sort is stable: equal ranks keep the order found.
      const best = ranked.filter((one) => !said.has(one.candidate.key) && !(one.candidate.key in claims) && options.exclude?.has(one.candidate.key) !== true).sort((a, b) => a.depth - b.depth || b.callers - a.callers)[0];
      return best === undefined ? [] : [best.candidate];
    };
    for (const edit of edits) {
      const at = locate(edit.file, store.repos);
      if (at === null) continue;
      if (isManifest(edit.file)) {
        if (edit.read) continue;
        const manifest = memo.text(edit.file);
        if (manifest !== null) isolated("pkg", () => packageLines(facts, at, checkouts, manifest, edit, memo));
        continue;
      }
      if (!SCANNED.test(edit.file)) continue;
      // The file's contract rows. Only the file's first contract line of the
      // session may add the rest of its operations (tier 3), which needs no
      // changed lines and no clean scan. A Read never says a contract line,
      // whatever the caller asked.
      let contractRows: ContractRow[] = [];
      let requestRows: ContractRow[] = [];
      if (options.contract === true && !edit.read) {
        try {
          const printed = facts.contracts(at.repo, at.rel).filter(printable);
          contractRows = printed.filter((row) => row.direction === "response");
          requestRows = printed.filter((row) => row.direction === "request");
        } catch (error) {
          log("contract fact lines failed", String(error));
          stopped("rule_failed");
        }
        const prefix = `contract::${at.repo}::${at.rel}::`;
        const earlier = (key: string) => key.startsWith(prefix) && !key.includes("::request::");
        const first = ![...said].some(earlier) && !(options.exclude && [...options.exclude].some(earlier));
        if (contractRows.length > 0 && first) rest.push({ at, rows: contractRows });
      }
      const byLine = edit.lines || edit.removed;
      // Changed lines not known (a Write over a file, a shell write, a patch
      // hunk that cannot be placed): nothing is said.
      if (!byLine && !edit.snippets && !edit.whole && !edit.read) {
        stopped("no_changed_lines");
        continue;
      }
      // A file over `MAX_FACT_FILE_BYTES` is generated or bundled: never read
      // in a hook's time. One that does not scan cleanly says nothing.
      const read = memo.scanned(edit.file);
      if (read === null) {
        log(`${at.rel} was not read cleanly; no fact lines for it`);
        stopped("not_read_cleanly");
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
      // The pairs a model found, keyed at a changed line of this file (never on a Read): each prints the sentence the cloud wrote.
      if (!edit.read) {
        isolated("matched", () =>
          facts
            .matched(at.repo, at.rel)
            .filter((row) => touchesLine(text, row.line, ranges, gaps))
            .slice(0, MAX_MATCHED_LINES)
            .map((row) => ({ key: `matched::${row.repo}::${row.pair}::${row.end}`, text: row.text, named: [] })),
        );
      }
      const reach = touched(text, scan, ranges, gaps);
      // A name declared twice in this file (a handler of one name in two
      // components) cannot say which one the edit reached.
      const declared = new Map<string, number>();
      for (const span of scan.spans) declared.set(span.name, (declared.get(span.name) ?? 0) + 1);
      const names = reach.names.filter((name) => declared.get(name) === 1);
      // The chain kind's lookup, after every file's tiers 1-2. A Read looks up
      // every named thing in the file and says the best one (`chainCandidates`).
      if (options.contract === true && options.chain === true && names.length > 0) {
        const start = new Map(scan.spans.map((span) => [span.name, span.start]));
        // Each name's enclosing named things, innermost first (once-declared names only).
        const outer = new Map<string, string[]>();
        for (const name of names) {
          const own = scan.spans.find((span) => span.name === name)!;
          const around = scan.spans.filter((span) => span !== own && span.start <= own.start && span.end >= own.end && declared.get(span.name) === 1);
          outer.set(name, around.sort((a, b) => a.end - a.start - (b.end - b.start)).map((span) => span.name));
        }
        chained.push({ at, names: [...names].sort((a, b) => start.get(a)! - start.get(b)!), outer, read: edit.read === true });
      }
      // A Read of the chain kind alone (the Read switch is off): no op, fn or name lookup, and the file is never answered.
      if (edit.read && options.chainOnly === true) continue;
      // The op rows this edit reaches: tier 1 of the contract kind.
      const reachedOps: FactRow[] = [];
      const opRan = isolated("op", () => {
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
      if (contractRows.length > 0 || requestRows.length > 0) {
        try {
          const reached = new Set(reachedOps.map(contractId));
          // Request rows: the same two tiers, their lines after every response line (tier 5).
          const askedOne = requestRows.filter((row) => reached.has(contractId(row)));
          for (const row of askedOne) taken.add(row);
          const askedTwo = requestRows.filter((row) => !taken.has(row) && bodyCalls(row, requestRows, names, scan));
          for (const row of askedTwo) taken.add(row);
          tiers[4]!.push(...byLineThenLabel([...askedOne, ...askedTwo]).flatMap((row) => contractCandidates(at, row)));
          const one = contractRows.filter((row) => reached.has(contractId(row)));
          for (const row of one) taken.add(row);
          const two = contractRows.filter((row) => !taken.has(row) && bodyCalls(row, contractRows, names, scan));
          for (const row of two) taken.add(row);
          for (const row of [...one, ...two]) takenIds.add(takenId(row));
          tiers[0]!.push(...byLineThenLabel(one).flatMap((row) => contractCandidates(at, row)));
          tiers[1]!.push(...byLineThenLabel(two).flatMap((row) => contractCandidates(at, row)));
        } catch (error) {
          log("contract fact lines failed", String(error));
          stopped("rule_failed");
        }
      }
      // The functions' callers in other files: only the row in this file.
      const fnRan = isolated("fn", () =>
        names.flatMap((name) =>
          facts
            .callers(at.repo, name)
            .filter((fn) => fn.file === at.rel)
            .map((fn) => ({
              key: `fn::${fn.repo}::${fn.file}::${fn.name}`,
              text: fn.text,
              named: pathsOf(fn.others),
              holds: peersHold(at, fn.others),
              // A Read's fn lines never stand in for the uses-count line of an edit.
              ...(edit.read ? {} : { file: edit.file }),
            })),
        ),
      );
      if (edit.read) {
        // No same-name line on a Read: every declaration would read as new.
        try {
          if (opRan && fnRan && facts.hasCallers() && (scan.spans.length > 0 || facts.unnamed(at.repo, at.rel).length > 0)) answered.push(edit.file);
        } catch (error) {
          log("read answer check failed", String(error));
          stopped("rule_failed");
        }
        continue;
      }
      isolated("name", () => sameNameLines(facts, at, checkouts, text, scan, edit, isJsx(edit.file), memo));
    }
    for (const one of chained) {
      try {
        tiers[2]!.push(...chainCandidates(one.at, one.names, one.outer, one.read));
      } catch (error) {
        log("chain fact lines failed", String(error));
        stopped("rule_failed");
      }
    }
    for (const one of rest) {
      try {
        tiers[3]!.push(...byLineThenLabel(one.rows.filter((row) => !taken.has(row))).flatMap((row) => contractCandidates(one.at, row)));
      } catch (error) {
        log("contract fact lines failed", String(error));
        stopped("rule_failed");
      }
    }
    const picked = pickLines({ ...found, contract: tiers.flat() }, said, claims, now, { limit: options.limit, exclude: options.exclude, stops });
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
    // Nothing printed, so nothing was answered: a Read keeps the binary path.
    answered.length = 0;
  } finally {
    store?.close();
  }
  return done();
}
