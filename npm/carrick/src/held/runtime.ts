// The real `ServiceRuntime` of the held-compiler daemon (part 3, T5b, L2).
//
// start:  write the service's shadow tree through the compiler engine, open a
//         TypeScript 7 language server over it, place every producer response
//         row's checks at the positions the index stored for its sends
//         (`values`), map those positions onto the current text and arm the
//         ones that pass the gate.
// answer: sync files that changed on disk, move each check with the edit, put
//         the edited file's text into the engine, pull diagnostics of every
//         check file and keep only a check's own (R3). After the answer, the
//         twin pass tests that each armed check can still fail, files whose
//         checks lost their place are gated again, and rows the fact file added
//         or changed are gated.
//
// Checks are keyed by the VALUE's file: a send can sit in another file than its
// route's. What an armed check IS is keyed by the row's identity, its group's
// type text and the position's index in the row's `values`.

import fs from "node:fs";
import path from "node:path";
import ts from "typescript";
import { textAtCommit } from "../facts/diff.ts";
import { contractKey } from "../facts/lines.ts";
import { commitsOf, indexedCommit, localCheckouts } from "../facts/peers.ts";
import { localNames, localRepos } from "../facts/repo-match.ts";
import { factsPath, openFacts, type ContractGroup, type ContractRow, type GraphqlTypeTable, type FactStore } from "../facts/store.ts";
import { realPath } from "../fs-util.ts";
import { createLogger } from "../log.ts";
import type { HeldLine, HeldRequest, ServiceEngine, ServiceInit, ServiceRuntime, StartOptions, StartResult } from "./daemon.ts";
import { StalePull, Ts7Lsp, type Diag as LspDiag, type Engine } from "./engine.ts";
import { heldRootOf, placedPath, shadowDir } from "./paths.ts";
import {
  arm,
  attribute,
  checkWire,
  isTopOrBytes,
  mapValue,
  overlay,
  regionsBetween,
  type CheckInput,
  type Diag,
  type GraphqlForm,
  type MessageForm,
  type Overlay,
  type Region,
  type Value,
} from "./place.ts";
import { runOf } from "./early.ts";
import { messageSendRows } from "./message-rows.ts";
import { heldBuildId } from "./build.ts";
import { cloneShadowOffLoop, engineEntry, writeShadow, writeShadowStamp, type CloneRefused, type FitOptions, type ShadowAnswer, type ShadowGraphql } from "./shadow.ts";
import { StepTimer, type Steps } from "./steps.ts";

const log = createLogger("carrick-held-runtime");

/** The longest each git call of the gate may take (a slower one is `index_text_unavailable`). */
const GIT_BUDGET_MS = 2000;

export type RuntimeDeps = {
  root: string;
  home?: string;
  env?: NodeJS.ProcessEnv;
  /** The built compiler engine's entry; found from the environment when unset. */
  entry?: string | null;
  /** The rows a check can sit on now: producer responses (HTTP, GraphQL) and socket emitters joined to their listeners. Default: the fact file's. */
  rows?: () => Promise<ContractRow[]>;
  /** A service's GraphQL SDL table now, or null. Default: the fact file's `gqltypes` row. */
  graphqlTypes?: (repo: string, service: string) => Promise<GraphqlTypeTable | null>;
  /** Each repo's index commit, as the fact file's header names it. Default: the fact file's. */
  commits?: () => Promise<Record<string, string> | null>;
  /** Where each repo is checked out here, as the fact file's `repos` table names it (the places of a line are named in their own repo). Default: the fact file's. */
  repoPaths?: () => Promise<Record<string, string>>;
  /** Changes when the rows may have: the fact file's size and mtime by default. */
  stamp?: () => string;
  /** The held compiler over a shadow's `w` directory. */
  makeEngine?: (command: string, cwd: string) => Engine;
};

type Unarmed = { label: string; protocol: string; file: string; reason: string; group: number; position: number | null; line?: number | null };
type Armed = { label: string; protocol: string; file: string; group: number; position: number; line?: number | null };

/** A GraphQL row's checks stay unarmed: the service's table is absent, or the compiler engine refused it. */
const GRAPHQL_TABLE_UNUSABLE = "graphql_table_unusable";
/** A GraphQL row's checks stay unarmed until the next start: the table or the roots the engine answered for changed (P15). */
const GRAPHQL_TABLE_CHANGED = "graphql_table_changed";

const isGraphql = (row: ContractRow): boolean => row.protocol === "graphql";
/** A socket emitter row: its check wraps the emit's payload and holds the listener's parameter type fixed. */
const isMessage = (row: ContractRow): boolean => row.protocol === "socket" && row.side === "consumer";
/** A socket row's checks stay unarmed: the compiler engine answered without the judge's message rules. */
const MESSAGE_RULES_MISSING = "message_rules_missing";

/** The roots a service's GraphQL rows answer, each once, in a stable order. */
function rootsOf(rows: ContractRow[]): ShadowGraphql["roots"] {
  const roots = new Map<string, ShadowGraphql["roots"][number]>();
  for (const row of rows) if (isGraphql(row) && row.graphql_root) roots.set(`${row.graphql_root.type}.${row.graphql_root.field}`, row.graphql_root);
  return [...roots].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)).map(([, root]) => ({ type: root.type, field: root.field }));
}

/** What the engine's GraphQL answer was given: a changed one means the answer no longer holds. */
const graphqlInput = (rows: ContractRow[], table: GraphqlTypeTable | null): string => JSON.stringify([table, rootsOf(rows)]);

/** The service's GraphQL checks: the declarations a file with one appends, each scalar-form root's leaf, and why none arms. */
type GraphqlChecks = { wire: string[]; leaves: Map<string, string>; reason: string | null; input: string };

/** A row's identity. Emitter rows of one event in one file differ only by their line, so theirs carries it; the others' stay as they were. */
const identity = (row: ContractRow): string => `${row.repo}\0${row.file}\0${row.label}\0${row.site ?? ""}${row.side === "consumer" ? `\0${row.side}\0${row.line ?? ""}` : ""}`;
/** A consumer row's call line, for its placed row and its held key; every other row adds nothing (HTTP and GraphQL keep their keys). */
const callLine = (row: ContractRow): { line?: number | null } => (row.side === "consumer" ? { line: row.line } : {});
/** A compiler message without the check's own names (`O<row>_<group>`, `send<row>_<group>`), which a re-placed overlay may renumber. */
const ownNamesOut = (message: string): string => message.replace(/\bO\d+_\d+\b/g, "O").replace(/\bsend\d+_\d+\b/g, "send");
/** The once-per-session key of a held line: `contractKey` plus, for a consumer row, its index line (two emits of one event are two lines). */
const heldKey = (row: ContractRow, g: number): string => `${contractKey(row, g)}${row.side === "consumer" ? `::line::${row.line ?? ""}` : ""}`;
const groupKey = (row: ContractRow, group: number): string => `${identity(row)}\0${row.groups[group]!.type}`;
/** One group at one position of its row's `values`. */
const checkKey = (row: ContractRow, group: number, at: number): string => `${groupKey(row, group)}\0${at}`;
const posId = (row: ContractRow, at: number): string => `${identity(row)}\0${at}`;

/** Lines and offsets of one text, TypeScript's own (the compiler states positions as line and character). */
function lineMap(text: string): { offset: (line: number, character: number) => number; line: (offset: number) => number } {
  const sf = ts.createSourceFile("held.ts", text, ts.ScriptTarget.Latest, false);
  return {
    offset: (line, character) => {
      try {
        return sf.getPositionOfLineAndCharacter(line, character);
      } catch {
        return text.length;
      }
    },
    line: (offset) => sf.getLineAndCharacterOfPosition(Math.min(Math.max(offset, 0), text.length)).line + 1,
  };
}

/** A compiler's line/character diagnostics as offsets into the exact text that was set. */
function offsetDiags(text: string, diags: LspDiag[] | undefined): Diag[] {
  const at = lineMap(text);
  return (diags ?? [])
    .filter((d) => d.severity === undefined || d.severity === 1)
    .map((d) => ({ start: at.offset(d.range.start.line, d.range.start.character), code: Number(d.code), message: d.message }));
}

/** A `sigs` value `sigOf` never returns: the file's disk text is newer than the engine's copy, so the next sweep applies it. */
const SNAPSHOT_SIG = "snapshot";

function readText(file: string): string | null {
  try {
    return fs.readFileSync(file, "utf8");
  } catch {
    return null;
  }
}

function writePlaced(file: string, service: string, armed: Armed[], unarmed: Unarmed[]): void {
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    fs.writeFileSync(file, JSON.stringify({ at: new Date().toISOString(), service, armed, unarmed }, null, 2));
  } catch (error) {
    log("could not write the placed record", String(error));
  }
}

/** Rows that state where their replies are sent from; the others are never placed. */
const hasValues = (row: ContractRow): boolean => (row.values?.length ?? 0) > 0;

/** One send position of a row. */
type Pos = { id: string; row: ContractRow; at: number };
/** A placed position: its bounds on the current text, and the groups it checks. */
type Entry = { pos: Pos; value: Value; groups: number[] };

type View = { ov: Overlay; inputs: CheckInput[]; entries: Entry[]; text: string };

/** A scalar-form GraphQL check: the only form with a declared fill, and so the only one the wire mode changes. */
const hasLeaf = (input: CheckInput): boolean => input.form?.kind === "graphql" && input.form.leaf !== null;
/** The reason `placed` gives a set-aside check (carrick#4940). */
const FIELD_RESOLVER_UNREAD = "field_resolver_unread";

/** Whether an unarmed reason may pass when the file is edited again. */
const transient = (reason: string): boolean => reason === "value_edited" || reason.startsWith("clean_check_fails");

/** One service's held compiler: its shadow, its checks and what is armed. */
class HeldService implements ServiceEngine {
  private readonly ownFiles: Set<string>;
  private readonly sigs = new Map<string, string>();
  /** Plain text of every file the engine holds open. */
  private readonly known = new Map<string, string>();
  /** The service's rows, and their positions by value file. */
  private rows: ContractRow[] = [];
  private readonly byFile = new Map<string, Pos[]>();
  /** Where each position is on `known`'s text. */
  private readonly values = new Map<string, Value>();
  /**
   * A consumer position's lead (HM4 D2): how many lines its payload's first
   * line sits below the call's line, at the index commit. The call's line on
   * the current text is the payload's line less it. Absent when the row states
   * no line or its payload sits in another file: that line cannot be joined.
   */
  private readonly leads = new Map<string, number>();
  private readonly armed = new Set<string>();
  private readonly unarmed = new Map<string, Unarmed>();
  /**
   * carrick#4940, per scalar-form GraphQL check: whether the judge's wire line
   * (the form without the declared fill) spoke at its callback the first time
   * the check armed. Kept across regates, so a check re-armed on an edited
   * text is compared with the text it first armed on.
   */
  private readonly wireAtGate = new Map<string, boolean>();
  /**
   * Armed checks the last fill step set aside (D-4): their wire line passed
   * at the gate and fails now while their clean check is silent, so the
   * declared fill alone keeps them silent, on a field resolver Carrick does
   * not read. They stay armed and in the view; `placed` counts them as not
   * checked, `field_resolver_unread`.
   */
  private setAside = new Set<string>();
  /** The checks whose clean form spoke at the edit answer in flight; null for any other request (no clean result to compare). */
  private spokeKeys: Set<string> | null = null;
  private readonly views = new Map<string, View>();
  /**
   * Positions, by file, that lost their place or may now be placed: gated again
   * after the answer. Only these positions are gated; the file's other checks
   * keep their state, so a file that is broken right now loses only the checks
   * it already lost.
   */
  private readonly regate = new Map<string, Set<string>>();
  /** The files of the service's program, as the compiler engine wrote them: the only files a check can sit in. */
  private readonly program: Set<string>;
  private stampSeen: string;
  private needsGate = false;
  /** Set when a request changed a file's text in the engine: the twin pass has something new to test. */
  private textMoved = false;
  /** Where the answer in flight spends its time; the after-answer work has its own (the queue runs them one at a time). */
  private timer = new StepTimer();
  private afterTimer = new StepTimer();
  /**
   * Per line (row, group), what its check spoke at the last edit answer that was
   * filed (T1 P1.1): the message without the check's own names, and the run it
   * belongs to, the answer it first spoke at. A line whose check spoke the same
   * message then is `standing`; a new engine starts empty, as its gate armed only
   * silent checks. An answer's map takes over only when the daemon commits it
   * (`answer`'s `commit`), so a superseded ask leaves the last filed one.
   */
  private spoke = new Map<string, { message: string; run: string }>();
  /** Edit answers computed so far; with the engine's id, names a run (`runOf`). */
  private answers = 0;
  /** This engine's id, one per start: a later engine's answers say nothing of this one's runs. */
  private readonly engineId = `${process.pid}-${Date.now()}`;

  private readonly init: ServiceInit;
  private readonly deps: Required<Pick<RuntimeDeps, "rows" | "stamp" | "commits" | "repoPaths" | "graphqlTypes">> & { root: string; home?: string; env: NodeJS.ProcessEnv };
  private readonly engine: Engine;
  private readonly w: string;
  private readonly wire: string[];
  private readonly gql: GraphqlChecks;
  /** The engine's message rules a file with a socket check appends; null when the engine gave none. */
  private readonly message: string[] | null;
  private readonly snapshot: Map<string, string | null>;

  constructor(
    init: ServiceInit,
    deps: Required<Pick<RuntimeDeps, "rows" | "stamp" | "commits" | "repoPaths" | "graphqlTypes">> & { root: string; home?: string; env: NodeJS.ProcessEnv },
    engine: Engine,
    w: string,
    ownFiles: string[],
    wire: string[],
    gql: GraphqlChecks,
    message: string[] | null,
    /** Each value file's text as it was when the start began (N2); the first gate arms on it, not on an edit made during the start. */
    snapshot: Map<string, string | null>,
  ) {
    this.snapshot = snapshot;
    this.init = init;
    this.deps = deps;
    this.engine = engine;
    this.w = w;
    this.wire = wire;
    this.gql = gql;
    this.message = message;
    this.ownFiles = new Set(ownFiles);
    this.program = new Set(ownFiles);
    for (const rel of this.ownFiles) this.sigs.set(rel, this.sigOf(rel));
    this.stampSeen = deps.stamp();
  }

  private real = (rel: string): string => path.join(this.init.scan_root, rel);
  private shadow = (rel: string): string => path.join(this.w, rel);

  private sigOf(rel: string): string {
    try {
      const st = fs.statSync(this.real(rel));
      return `${st.mtimeMs}:${st.size}`;
    } catch {
      return "gone";
    }
  }

  loadRows(rows: ContractRow[]): void {
    this.byFile.clear();
    this.rows = rows.filter((row) => row.repo === this.init.repo && row.service === this.init.service && row.direction === "response");
    const live = new Set<string>();
    const liveKeys = new Set<string>();
    for (const row of this.rows) {
      (row.values ?? []).forEach((v, at) => {
        const pos: Pos = { id: posId(row, at), row, at };
        live.add(pos.id);
        row.groups.forEach((_, g) => liveKeys.add(checkKey(row, g, at)));
        this.byFile.set(v.file, [...(this.byFile.get(v.file) ?? []), pos]);
      });
    }
    for (const id of [...this.values.keys()]) if (!live.has(id)) this.values.delete(id);
    for (const id of [...this.leads.keys()]) if (!live.has(id)) this.leads.delete(id);
    for (const key of [...this.wireAtGate.keys()]) if (!liveKeys.has(key)) this.wireAtGate.delete(key);
    for (const key of [...this.setAside]) if (!liveKeys.has(key)) this.setAside.delete(key);
  }

  /** The text of `rel` at the index commit and the regions `text` (the text being gated) changed it by; or why neither is known. */
  private baseline(rel: string, text: string): { old: string; regions: Region[] } | "index_text_unavailable" {
    const commit = indexedCommit(this.commits, this.init.repo);
    if (commit === null) return "index_text_unavailable";
    const real = this.real(rel);
    const old = textAtCommit(real, commit, this.deps.env, GIT_BUDGET_MS);
    if (old === null) return "index_text_unavailable";
    if (old === text) return { old, regions: [] };
    return { old, regions: regionsBetween(old, text) };
  }

  private commits: Record<string, string> | null = null;
  private repoPaths: Record<string, string> = {};

  /** D6: place every position of every file on the current text and arm what passes. `onlyNew`: leave decided checks alone. */
  async gate(onlyNew: boolean): Promise<void> {
    this.commits = await this.deps.commits();
    this.repoPaths = await this.deps.repoPaths();
    for (const rel of this.byFile.keys()) await this.gateFile(rel, onlyNew);
    for (const rel of this.byFile.keys()) this.setView(rel);
    for (const rel of [...this.views.keys()]) if (!this.byFile.has(rel)) this.views.delete(rel);
    this.writePlaced();
  }

  /** Gate again after the answer: these positions of `rel`. */
  private queue(rel: string, ids: Iterable<string>): void {
    const held = this.regate.get(rel) ?? new Set<string>();
    for (const id of ids) held.add(id);
    this.regate.set(rel, held);
  }

  /**
   * Place and arm the positions of `rel` (only `ids`, when given). The gate
   * reads the disk, so it never runs on text newer than the engine's own copy
   * (`known`): that edit is not applied yet, and gating it would arm or unarm
   * checks on a file the answer has not seen. Those positions wait for the
   * next answer.
   */
  private async gateFile(rel: string, onlyNew: boolean, ids: Set<string> | null = null): Promise<void> {
    const positions = (this.byFile.get(rel) ?? []).filter((p) => ids === null || ids.has(p.id));
    const keysOf = (p: Pos): string[] => p.row.groups.map((_, g) => checkKey(p.row, g, p.at));
    const decided = (key: string): boolean => this.armed.has(key) || this.unarmed.has(key);
    if (onlyNew && positions.every((p) => keysOf(p).every(decided))) return;
    const no = (p: Pos, g: number, reason: string): void => {
      const key = checkKey(p.row, g, p.at);
      if (onlyNew && decided(key)) return;
      this.armed.delete(key);
      this.unarmed.set(key, { label: p.row.label, protocol: p.row.protocol, file: rel, reason, group: g, position: p.at, ...callLine(p.row) });
    };
    const noneFor = (reason: string): void => {
      for (const p of positions) p.row.groups.forEach((_, g) => no(p, g, reason));
    };
    // A check can only sit in a file of the program the compiler engine loaded.
    if (!this.program.has(rel)) return noneFor("value_outside_service");
    const disk = readText(this.real(rel));
    // N2: the first gate of a file judges the text the start began with. The next ask applies the disk text as an edit.
    const taken = this.known.has(rel) ? undefined : this.snapshot.get(rel);
    this.snapshot.delete(rel);
    const text = taken != null && taken !== disk ? taken : disk;
    if (text !== disk) this.sigs.set(rel, SNAPSHOT_SIG);
    const held = this.known.get(rel);
    if (text !== null && held !== undefined && held !== text) return this.queue(rel, positions.map((p) => p.id));
    if (!onlyNew) {
      for (const p of positions) for (const key of keysOf(p)) {
        this.armed.delete(key);
        this.unarmed.delete(key);
      }
    }
    if (text === null) return noneFor("file_unreadable");
    const base = this.baseline(rel, text);
    if (typeof base === "string") return noneFor(base);
    const entries: Entry[] = [];
    const oldLines = positions.some((p) => p.row.side === "consumer") ? lineMap(base.old) : null;
    for (const p of positions) {
      if (oldLines !== null && p.row.side === "consumer") {
        const lead = p.row.line === null || this.real(rel) !== path.join(this.init.repo_path, p.row.file) ? -1 : oldLines.line(p.row.values![p.at]!.span[0]) - p.row.line;
        if (lead >= 0) this.leads.set(p.id, lead);
        else this.leads.delete(p.id);
      }
      if (isGraphql(p.row) && this.gql.reason !== null) {
        p.row.groups.forEach((_, g) => no(p, g, this.gql.reason!));
        continue;
      }
      if (isMessage(p.row) && this.message === null) {
        p.row.groups.forEach((_, g) => no(p, g, MESSAGE_RULES_MISSING));
        continue;
      }
      const span = p.row.values![p.at]!.span;
      // A value an edit crossed is never placed by a guess: it waits for a text that places it by arithmetic again.
      const placed = mapValue({ file: rel, start: span[0], end: span[1] }, base.regions);
      if (placed === "crossed") {
        p.row.groups.forEach((_, g) => no(p, g, "value_edited"));
        continue;
      }
      this.values.set(p.id, placed);
      // A GraphQL check never arms on a stated top type or bytes (L-G1a): the engine's form composes none of the judge's gates for them.
      // A socket check neither: a listener typed any, unknown, never or bytes states no contract the held form reads.
      const typed = p.row.groups.map((_, g) => g).filter((g) => !((isGraphql(p.row) || isMessage(p.row)) && isTopOrBytes(p.row.groups[g]!.type)));
      p.row.groups.forEach((_, g) => { if (!typed.includes(g)) no(p, g, "top_type"); });
      // A caller type declared in a file this program also compiles moves with the value on an edit to it, while the check holds its text (T3, #4658).
      const groups = typed.filter((g) => !this.sharedHome(p.row, g));
      typed.forEach((g) => { if (!groups.includes(g)) no(p, g, "shared_home"); });
      if (groups.length > 0) entries.push({ pos: p, value: placed, groups });
    }
    const shadow = this.shadow(rel);
    const inputs = this.inputsOf(entries);
    const clean = this.overlayOf(rel, text, inputs, "clean");
    for (const u of clean.unplaced) {
      const e = entries[u.n]!;
      e.groups.forEach((g) => no(e.pos, g, u.reason));
    }
    if (clean.checks.length > 0) {
      try {
        this.engine.set(shadow, clean.text);
        const cleanDiags = offsetDiags(clean.text, (await this.engine.pull([shadow])).get(shadow));
        const twin = this.overlayOf(rel, text, inputs, "twin");
        this.engine.set(shadow, twin.text);
        const twinDiags = offsetDiags(twin.text, (await this.engine.pull([shadow])).get(shadow));
        const verdict = arm(clean, cleanDiags, twinDiags);
        const arming = verdict.armed
          .map((k) => {
            const c = clean.checks.find((x) => x.k === k)!;
            const e = entries[c.n]!;
            return { k, input: inputs[c.n]!, key: checkKey(e.pos.row, e.groups[c.group]!, e.pos.at) };
          })
          .filter((a) => !(onlyNew && decided(a.key)));
        // carrick#4940: a scalar-form GraphQL check arming for the first time records whether the judge's wire line speaks on this text.
        const fresh = arming.filter((a) => hasLeaf(a.input) && !this.wireAtGate.has(a.key));
        if (fresh.length > 0) {
          const wire = this.overlayOf(rel, text, inputs, "wire");
          this.engine.set(shadow, wire.text);
          const spoke = attribute(wire, offsetDiags(wire.text, (await this.engine.pull([shadow])).get(shadow))).lines;
          for (const a of fresh) this.wireAtGate.set(a.key, spoke.has(a.k));
        }
        for (const { key } of arming) {
          this.armed.add(key);
          this.unarmed.delete(key);
        }
        for (const u of verdict.unarmed) {
          const e = entries[u.n]!;
          no(e.pos, e.groups[u.group]!, u.reason);
        }
      } catch (error) {
        // No report for the text sent: these positions arm nothing and keep no reason; they are gated again after the next answer.
        if (!(error instanceof StalePull)) throw error;
        log("gate pull stale", rel, String(error));
        this.queue(rel, positions.map((p) => p.id));
      } finally {
        this.engine.set(shadow, text);
      }
    }
    this.known.set(rel, text);
  }

  /**
   * The group's home is a file of this engine's program that is not one of the group's own place files: a declaration both ends compile.
   * `home.file` and a place's file are relative to the place's repo; a home in another repo is never in this program.
   */
  private sharedHome(row: ContractRow, g: number): boolean {
    const group = row.groups[g]!;
    if (!group.home) return false;
    const here = group.places.filter((pl) => pl.repo === this.init.repo);
    if (here.length === 0 || here.some((pl) => pl.file === group.home!.file)) return false;
    return this.program.has(path.relative(this.init.scan_root, path.join(this.init.repo_path, group.home.file)));
  }

  /** The checks' inputs: one per entry, numbered by its place in `entries`. */
  private inputsOf(entries: Entry[]): CheckInput[] {
    return entries.map((e, n) => {
      const form = this.formOf(e.pos.row);
      return { n, value: e.value, groups: e.groups.map((g) => ({ type: e.pos.row.groups[g]!.type })), ...(form ? { form } : {}) };
    });
  }

  /** A GraphQL row's form: the scalar form where the engine gave its root a leaf, else the JSON form (P5). A socket emitter's: the message form. */
  private formOf(row: ContractRow): GraphqlForm | MessageForm | undefined {
    if (isMessage(row)) return { kind: "message" };
    if (!isGraphql(row)) return undefined;
    const root = row.graphql_root;
    return { kind: "graphql", leaf: (root && this.gql.leaves.get(`${root.type}.${root.field}`)) ?? null };
  }

  /**
   * `overlay` with the wire its checks read, per kind: a file with a GraphQL
   * check also appends the engine's GraphQL rules, a file with a socket check
   * the engine's message rules, a file with both appends both; a file with
   * neither keeps its text.
   */
  private overlayOf(rel: string, text: string, inputs: CheckInput[], mode: "clean" | "twin" | "wire"): Overlay {
    const base = inputs.some((i) => i.form?.kind === "graphql") ? this.gql.wire : this.wire;
    const wire = inputs.some((i) => i.form?.kind === "message") ? [...base, ...(this.message ?? [])] : base;
    return overlay(this.shadow(rel), text, inputs, wire, mode);
  }

  /** The armed groups of each position of `rel`, placed where they are now. */
  private armedEntries(rel: string): Entry[] {
    const out: Entry[] = [];
    for (const pos of this.byFile.get(rel) ?? []) {
      const value = this.values.get(pos.id);
      const groups = pos.row.groups.map((_, g) => g).filter((g) => this.armed.has(checkKey(pos.row, g, pos.at)));
      if (value !== undefined && groups.length > 0) out.push({ pos, value, groups });
    }
    return out;
  }

  /** Put `rel`'s text into the engine, with its armed checks wrapped when it has any. */
  private setView(rel: string): void {
    const text = this.known.get(rel);
    if (text === undefined) return;
    const entries = this.armedEntries(rel);
    if (entries.length === 0) {
      this.views.delete(rel);
      this.engine.set(this.shadow(rel), text);
      return;
    }
    const inputs = this.inputsOf(entries);
    const ov = this.overlayOf(rel, text, inputs, "clean");
    this.engine.set(this.shadow(rel), ov.text);
    this.views.set(rel, { ov, inputs, entries, text });
  }

  private writePlaced(): void {
    const armed: Armed[] = [];
    const unarmed: Unarmed[] = [];
    for (const row of this.rows) {
      if (!hasValues(row)) {
        row.groups.forEach((_, g) => unarmed.push({ label: row.label, protocol: row.protocol, file: row.file, reason: "no_values", group: g, position: null, ...callLine(row) }));
        continue;
      }
      row.values!.forEach((v, at) => {
        row.groups.forEach((_, g) => {
          const key = checkKey(row, g, at);
          if (this.armed.has(key) && this.setAside.has(key)) unarmed.push({ label: row.label, protocol: row.protocol, file: v.file, reason: FIELD_RESOLVER_UNREAD, group: g, position: at, ...callLine(row) });
          else if (this.armed.has(key)) armed.push({ label: row.label, protocol: row.protocol, file: v.file, group: g, position: at, ...callLine(row) });
          else if (this.unarmed.has(key)) unarmed.push(this.unarmed.get(key)!);
        });
      });
    }
    writePlaced(placedPath(heldRootOf(this.init, this.deps.root), this.init.service, this.deps.home), this.init.service, armed, unarmed);
  }

  /** The positions of `rel` that are unarmed for a reason an edit can change. */
  private transientIds(rel: string): string[] {
    return (this.byFile.get(rel) ?? [])
      .filter((p) => p.row.groups.some((_, g) => transient(this.unarmed.get(checkKey(p.row, g, p.at))?.reason ?? "")))
      .map((p) => p.id);
  }

  /**
   * Move every position of `rel` with the edit from `old` to `text`, by the
   * region of each run of changed lines: however many edits the two texts are
   * apart, a value no edit crossed moves by arithmetic. One an edit crossed is
   * dropped, never placed by a guess (its check would speak on whatever node it
   * landed on), and the file is gated again.
   */
  private remap(rel: string, old: string, text: string): void {
    const regions = regionsBetween(old, text);
    for (const pos of this.byFile.get(rel) ?? []) {
      const value = this.values.get(pos.id);
      if (value === undefined) continue;
      const moved = mapValue(value, regions);
      if (moved === "crossed") {
        this.values.delete(pos.id);
        this.queue(rel, [pos.id]);
      } else {
        this.values.set(pos.id, moved);
      }
    }
  }

  /** Make the shadow and the engine agree with the disk for `rel`. */
  private async apply(rel: string): Promise<void> {
    const real = this.real(rel);
    const text = readText(real);
    if (text === null) {
      this.ownFiles.delete(rel);
      this.sigs.delete(rel);
      fs.rmSync(this.shadow(rel), { force: true });
      this.textMoved = true;
      return;
    }
    this.ownFiles.add(rel);
    this.sigs.set(rel, this.sigOf(rel));
    const old = this.known.get(rel);
    if (old === text) return;
    // A file the engine has not been told about yet, still as the shadow copied it, moves no text.
    const unmoved = old === undefined && readText(this.shadow(rel)) === text;
    fs.mkdirSync(path.dirname(this.shadow(rel)), { recursive: true });
    fs.writeFileSync(this.shadow(rel), text);
    this.known.set(rel, text);
    if (!unmoved) this.textMoved = true;
    if (this.byFile.has(rel)) {
      if (old !== undefined) await this.timer.run("remap", () => this.remap(rel, old, text));
      this.queue(rel, this.transientIds(rel));
      await this.timer.run("view", () => this.setView(rel));
    } else this.engine.set(this.shadow(rel), text);
  }

  /**
   * D9: take what moved on disk behind the daemon, then the files this request
   * names. A moved file is opened in the engine, not left to the language
   * server's own disk watch: that watch lagged a fraction of a second behind in
   * measurement, which would answer with the file as it was.
   */
  private async sync(requested: string[]): Promise<void> {
    const moved = await this.timer.run("sweep", () => [...this.ownFiles].filter((rel) => !requested.includes(rel) && this.sigOf(rel) !== this.sigs.get(rel)));
    for (const rel of moved) await this.apply(rel);
    for (const rel of requested) await this.apply(rel);
  }

  private async refreshRows(): Promise<void> {
    const stamp = this.deps.stamp();
    if (stamp === this.stampSeen) return;
    this.stampSeen = stamp;
    this.loadRows(await this.deps.rows());
    this.needsGate = true;
    if (this.gql.reason === null && this.rows.some(isGraphql)) {
      const now = graphqlInput(this.rows, await this.deps.graphqlTypes(this.init.repo, this.init.service));
      if (now !== this.gql.input) this.unarmGraphql(GRAPHQL_TABLE_CHANGED);
    }
  }

  /** P15: the engine's GraphQL answer no longer holds; every GraphQL check is unarmed until the next start. */
  private unarmGraphql(reason: string): void {
    this.gql.reason = reason;
    const files = new Set<string>();
    for (const row of this.rows.filter(isGraphql)) {
      (row.values ?? []).forEach((v, at) => {
        files.add(v.file);
        row.groups.forEach((_, g) => {
          const key = checkKey(row, g, at);
          this.armed.delete(key);
          this.unarmed.set(key, { label: row.label, protocol: row.protocol, file: v.file, reason, group: g, position: at, ...callLine(row) });
        });
      });
    }
    for (const rel of files) this.setView(rel);
  }

  async answer(files: string[], request: HeldRequest): Promise<{ lines: HeldLine[]; steps: Steps; after: () => Promise<Steps>; commit?: () => void; engine?: string }> {
    this.timer = new StepTimer();
    this.spokeKeys = null;
    await this.timer.run("refresh", () => this.refreshRows());
    const requested = files
      .map((file) => path.relative(this.init.scan_root, file))
      .filter((rel) => rel !== "" && !rel.startsWith("..") && !path.isAbsolute(rel));
    await this.sync(requested);
    if (request.kind !== "edit") return { lines: [], steps: this.timer.steps(), after: () => this.afterAnswer() };
    const { lines, spoke } = await this.lines();
    return { lines, steps: this.timer.steps(), after: () => this.afterAnswer(), commit: () => (this.spoke = spoke), engine: this.engineId };
  }

  /** Every view's lines, each marked against what the last filed answer spoke, and what this answer spoke (to commit when filed). */
  private async lines(): Promise<{ lines: HeldLine[]; spoke: Map<string, { message: string; run: string }> }> {
    const rels = [...this.views.keys()];
    const spokeKeys = new Set<string>();
    if (rels.length === 0) {
      this.spokeKeys = spokeKeys;
      return { lines: [], spoke: new Map() };
    }
    const pulled = await this.timer.run("pull", () => this.engine.pull(rels.map((rel) => this.shadow(rel))));
    this.spokeKeys = spokeKeys;
    const out: HeldLine[] = [];
    const said = new Set<string>();
    const run = runOf(this.engineId, ++this.answers);
    const spoke = new Map<string, { message: string; run: string }>();
    for (const rel of rels) {
      const view = this.views.get(rel)!;
      const at = lineMap(view.ov.text);
      let plain: ReturnType<typeof lineMap> | null = null;
      /** A consumer entry's call line on the current text, or null when it has no lead. */
      const callLineOf = (entry: Entry): number | null => {
        const lead = this.leads.get(entry.pos.id);
        if (lead === undefined) return null;
        plain ??= lineMap(view.text);
        return plain.line(entry.value.start) - lead;
      };
      const own = attribute(view.ov, offsetDiags(view.ov.text, pulled.get(this.shadow(rel))));
      // One line per (row, group), at the first position that broke.
      const broken = [...own.lines].map(([k, diags]) => ({ check: view.ov.checks.find((c) => c.k === k)!, diags })).sort((a, b) => a.diags[0]!.start - b.diags[0]!.start);
      for (const { check, diags } of broken) {
        const entry = view.entries[check.n]!;
        const g = entry.groups[check.group]!;
        const row = entry.pos.row;
        spokeKeys.add(checkKey(row, g, entry.pos.at));
        const key = `${heldKey(row, g)}\0${g}`;
        if (said.has(key)) continue;
        said.add(key);
        const first = diags[0]!;
        const message = ownNamesOut(first.message);
        const before = this.spoke.get(key);
        const standing = before !== undefined && before.message === message;
        spoke.set(key, { message, run: standing ? before.run : run });
        out.push({
          row_key: heldKey(row, g),
          group: g,
          label: row.label,
          consumer: [...new Set(row.groups[g]!.places.map((p) => p.service))].join(", "),
          file: rel,
          line: at.line(first.start),
          message: first.message,
          symbol: row.groups[g]!.symbol,
          row_file: row.file,
          row_line: row.line,
          row_path: path.join(this.init.repo_path, row.file),
          places: row.groups[g]!.places.map((p) => ({ service: p.service, file: p.file, line: p.line })),
          expected: row.groups[g]!.type,
          ...this.otherSide(row.groups[g]!),
          // A consumer line names its protocol (the sentence's other side) and its call's line (part 2's join, HM4 D2).
          ...(isMessage(row) ? { side: "consumer" as const, protocol: row.protocol, call_line: callLineOf(entry) } : {}),
          run: spoke.get(key)!.run,
          ...(standing ? { standing: true as const } : {}),
        });
      }
    }
    return { lines: out, spoke };
  }

  /**
   * The copy of the other side a line's check read (T2 P2.1, P2.2): the commit of
   * main the fact file names for the places' repo (the first place that has one),
   * and the files that hold it, absolute: each place's file and the group's home
   * file, both named in the place's repo. A repo with no checkout here names no file.
   */
  private otherSide(group: ContractGroup): { main_commit: string | null; named_files: string[] } {
    const checkouts = localCheckouts(new Map(Object.entries(this.repoPaths)));
    const top = (repo: string): string | null => checkouts.place(repo, { repo: this.init.repo, top: this.init.repo_path })?.top ?? null;
    const files = group.places.flatMap((p) => {
      const base = top(p.repo);
      return base === null ? [] : [path.join(base, p.file)];
    });
    const homeBase = !group.home || group.places.length === 0 ? null : top(group.places[0]!.repo);
    if (group.home && homeBase !== null) files.push(path.join(homeBase, group.home.file));
    const main = group.places.map((p) => indexedCommit(this.commits, p.repo)).find((commit) => commit !== null) ?? null;
    return { main_commit: main, named_files: [...new Set(files)] };
  }

  /**
   * The twin pass, restored in a `finally`; then files that lost a check's place, then rows the fact file added or changed;
   * last, after an edit answer that moved anything, the fill step (so a check the regate re-armed at this edit is compared too).
   */
  private async afterAnswer(): Promise<Steps> {
    this.afterTimer = new StepTimer();
    let moved = false;
    // The twin pass tests texts; a request that changed none (a read of an unchanged file) has nothing new for it, and
    // the next request would wait behind it for a second and more on a large service.
    if (this.textMoved) {
      this.textMoved = false;
      moved = true;
      await this.afterTimer.run("twin", () => this.twinPass());
    }
    if (this.regate.size > 0) {
      moved = true;
      await this.afterTimer.run("regate", () => this.regatePending());
    }
    if (this.needsGate) {
      this.needsGate = false;
      moved = true;
      await this.afterTimer.run("gate", () => this.gate(true));
    }
    const spoke = this.spokeKeys;
    if (moved && spoke !== null) await this.afterTimer.run("fill", () => this.fillPass(spoke));
    return this.afterTimer.steps();
  }

  /**
   * carrick#4940 (D-3, D-4): every view with a scalar-form GraphQL check goes in with the judge's wire line in place of the
   * declared line, one pull answers for all of them, and every clean text goes back in a `finally`, as the twin pass does.
   * A check is set aside when it is armed, its wire line was silent when it first armed, its wire line speaks at its
   * callback now, and its clean check did not speak at this answer (`spoke`): only the declared fill keeps it silent, on a
   * field the record holds as a plain value where the schema declares an object, which a field resolver Carrick does not
   * read may or may not convert. A check whose wire line already spoke when it armed is never set aside (#4951).
   */
  private async fillPass(spoke: Set<string>): Promise<void> {
    const views = [...this.views]
      .filter(([, view]) => view.inputs.some(hasLeaf))
      .map(([rel, view]) => ({ rel, view, shadow: this.shadow(rel), wire: this.overlayOf(rel, view.text, view.inputs, "wire") }));
    const next = new Set<string>();
    if (views.length > 0) {
      try {
        for (const v of views) this.engine.set(v.shadow, v.wire.text);
        const pulled = await this.engine.pull(views.map((v) => v.shadow));
        for (const v of views) {
          const own = attribute(v.wire, offsetDiags(v.wire.text, pulled.get(v.shadow))).lines;
          for (const c of v.wire.checks) {
            if (!own.has(c.k) || !hasLeaf(v.view.inputs[c.n]!)) continue;
            const entry = v.view.entries[c.n]!;
            const key = checkKey(entry.pos.row, entry.groups[c.group]!, entry.pos.at);
            if (this.armed.has(key) && this.wireAtGate.get(key) === false && !spoke.has(key)) next.add(key);
          }
        }
      } catch (error) {
        // No report for the text sent: what was set aside stays so until the next edit answer.
        if (!(error instanceof StalePull)) throw error;
        log("fill pull stale", String(error));
        return;
      } finally {
        for (const v of views) this.engine.set(v.shadow, v.view.ov.text);
      }
    }
    const changed = next.size !== this.setAside.size || [...next].some((key) => !this.setAside.has(key));
    this.setAside = next;
    if (changed) this.writePlaced();
  }

  /**
   * Every view's twin text goes in, one pull answers for all of them (the compiler
   * checks the files side by side), and every clean text goes back in a `finally`.
   * A twin only adds a property to what a check states, so no file's twin changes
   * what another file's diagnostics say.
   */
  private async twinPass(): Promise<void> {
    const twins = [...this.views].map(([rel, view]) => {
      const shadow = this.shadow(rel);
      return { rel, view, shadow, twin: this.overlayOf(rel, view.text, view.inputs, "twin") };
    });
    if (twins.length === 0) return;
    try {
      for (const t of twins) this.engine.set(t.shadow, t.twin.text);
      const pulled = await this.engine.pull(twins.map((t) => t.shadow));
      for (const t of twins) {
        const diags = offsetDiags(t.twin.text, pulled.get(t.shadow));
        for (const lost of arm(t.view.ov, [], diags).unarmed) log(`twin_lost ${this.init.service} ${t.rel} ${lost.k}`);
      }
    } finally {
      for (const t of twins) this.engine.set(t.shadow, t.view.ov.text);
    }
  }

  private async regatePending(): Promise<void> {
    const pending = [...this.regate];
    this.regate.clear();
    this.commits = await this.deps.commits();
    this.repoPaths = await this.deps.repoPaths();
    for (const [rel, ids] of pending) {
      await this.gateFile(rel, false, ids);
      this.setView(rel);
    }
    this.writePlaced();
  }

  rss(): number | null {
    return this.engine.rss();
  }

  close(): Promise<void> {
    return this.engine.close();
  }
}

/** Whether the service's options and the judge's agree on everything that decides whether one type fits another. */
function sameFitOptions(a: FitOptions, b: FitOptions): boolean {
  return a.strictNullChecks === b.strictNullChecks && a.strictFunctionTypes === b.strictFunctionTypes && a.exactOptionalPropertyTypes === b.exactOptionalPropertyTypes;
}

/** A service that holds no compiler: no route framework, or no row to check. */
class IdleService implements ServiceEngine {
  answer = async (): Promise<{ lines: HeldLine[] }> => ({ lines: [] });
  rss = (): number | null => null;
  holdsCompiler = false;
  close = async (): Promise<void> => undefined;
}

function factStamp(root: string): string {
  try {
    const st = fs.statSync(factsPath(root));
    return `${st.mtimeMs}:${st.size}`;
  } catch {
    return "none";
  }
}

/**
 * The names this checkout calls the fact file's repos (`facts/repo-match.ts`):
 * the daemon keys a service by its local name (the folder's), the fact file by
 * the hosted one. Empty when every repo's folder is named as it is.
 */
function renames(root: string, store: FactStore): Map<string, string> {
  return localNames(store.repos, localRepos(root), Object.keys(commitsOf(store.meta) ?? {}));
}

const named = (names: ReadonlyMap<string, string>, repo: string): string => names.get(repo) ?? repo;

async function factCommits(root: string): Promise<Record<string, string> | null> {
  const store = await openFacts(root);
  if (store === null) return null;
  try {
    const names = renames(root, store);
    const commits = commitsOf(store.meta);
    return commits === null ? null : Object.fromEntries(Object.entries(commits).map(([repo, commit]) => [named(names, repo), commit]));
  } finally {
    store.close();
  }
}

async function factRepoPaths(root: string): Promise<Record<string, string>> {
  const store = await openFacts(root);
  if (store === null) return {};
  try {
    const names = renames(root, store);
    return Object.fromEntries(store.repos.map((entry) => [named(names, entry.repo), entry.path]));
  } finally {
    store.close();
  }
}

async function factRows(root: string): Promise<ContractRow[]> {
  const store = await openFacts(root);
  if (store === null) return [];
  try {
    const rows = [...store.producerResponses(), ...store.graphqlProducerResponses(), ...messageSendRows(store.messageRows())];
    const names = renames(root, store);
    if (names.size === 0) return rows;
    const at = (repo: string) => named(names, repo);
    return rows.map((row) => ({
      ...row,
      repo: at(row.repo),
      groups: row.groups.map((group) => ({ ...group, places: group.places.map((place) => ({ ...place, repo: at(place.repo) })) })),
      untyped: row.untyped.map((entry) => ({ ...entry, repo: at(entry.repo) })),
    }));
  } finally {
    store.close();
  }
}

async function factTable(root: string, repo: string, service: string): Promise<GraphqlTypeTable | null> {
  const store = await openFacts(root);
  if (store === null) return null;
  try {
    const hosted = [...renames(root, store)].find(([, local]) => local === repo)?.[0] ?? repo;
    return store.graphqlTypes(hosted, service);
  } finally {
    store.close();
  }
}

/** Every row of `rows` unarmed, each for the reason `why` gives it. */
const allUnarmed = (rows: ContractRow[], why: (row: ContractRow) => string): Unarmed[] =>
  rows.flatMap((row) => row.groups.map((_, g) => ({ label: row.label, protocol: row.protocol, file: row.file, reason: why(row), group: g, position: null, ...callLine(row) })));

export function createRuntime(options: RuntimeDeps): ServiceRuntime {
  const env = options.env ?? process.env;
  const deps = {
    root: options.root,
    ...(options.home === undefined ? {} : { home: options.home }),
    rows: options.rows ?? (() => factRows(options.root)),
    stamp: options.stamp ?? (() => factStamp(options.root)),
    commits: options.commits ?? (() => factCommits(options.root)),
    repoPaths: options.repoPaths ?? (() => factRepoPaths(options.root)),
    graphqlTypes: options.graphqlTypes ?? ((repo: string, service: string) => factTable(options.root, repo, service)),
    env,
  };
  // The rows `start` reads for a service, counted once per fact-file stamp.
  let counted: { stamp: string; counts: Map<string, number> } | null = null;
  /**
   * A linked worktree's shadow (#5038, T2): a clone of the main checkout's shadow for the same service,
   * never a write of its own. The clone's tsconfig reads main's `v/` in place.
   */
  const cloneOf = (init: ServiceInit & { checkout: string }, out: string): Promise<ShadowAnswer | CloneRefused> => {
    const top = realPath(init.checkout);
    const scanRel = path.relative(top, realPath(init.scan_root));
    return cloneShadowOffLoop(shadowDir(options.root, init.service, options.home), top, scanRel, out, { buildId: heldBuildId() });
  };
  const runtime = {
    async rowCount(init: ServiceInit): Promise<number> {
      const stamp = deps.stamp();
      if (counted === null || counted.stamp !== stamp) {
        const counts = new Map<string, number>();
        for (const row of await deps.rows()) {
          if (row.direction !== "response") continue;
          const k = `${row.repo}\0${row.service}`;
          counts.set(k, (counts.get(k) ?? 0) + 1);
        }
        counted = { stamp, counts };
      }
      return counted.counts.get(`${init.repo}\0${init.service}`) ?? 0;
    },
    async start(init: ServiceInit, startOptions: StartOptions = {}): Promise<StartResult> {
      const entry = options.entry === undefined ? engineEntry(env) : options.entry;
      if (entry === null) return { state: "error", reason: "the compiler engine is not built" };
      const out = shadowDir(heldRootOf(init, options.root), init.service, options.home);
      fs.rmSync(out, { recursive: true, force: true });
      const mine = (await deps.rows()).filter((row) => row.repo === init.repo && row.service === init.service && row.direction === "response");
      // N2: the value files as they are now, before the shadow is written and the engine starts; an edit made during the start is not in them.
      const snapshot = new Map<string, string | null>();
      for (const row of mine) for (const v of row.values ?? []) if (!snapshot.has(v.file)) snapshot.set(v.file, readText(path.join(init.scan_root, v.file)));
      // A worktree engine started by an edit: its first gate judges the edited files as the worktree's HEAD has them, so the
      // edit that started it is then judged as an edit (T2 F1). A file HEAD lacks has no such text and is gated as it is.
      if (init.checkout !== undefined) {
        for (const file of startOptions.edited ?? []) {
          const rel = path.relative(init.scan_root, realPath(file));
          if (!snapshot.has(rel)) continue;
          const head = textAtCommit(path.join(init.scan_root, rel), "HEAD", deps.env, GIT_BUDGET_MS);
          if (head !== null) snapshot.set(rel, head);
        }
      }
      // GraphQL rows (P5, P15): the engine is told the service's table and roots once, here. With no table, or one the
      // engine refuses, they stay unarmed with one reason, and the shadow is written as for HTTP alone.
      const table = mine.some(isGraphql) ? await deps.graphqlTypes(init.repo, init.service) : null;
      const ask: ShadowGraphql | undefined = table === null ? undefined : { types: table, roots: rootsOf(mine) };
      let graphqlReason = mine.some(isGraphql) && table === null ? GRAPHQL_TABLE_UNUSABLE : null;
      let shadow: ShadowAnswer;
      if (init.checkout !== undefined) {
        const cloned = await cloneOf({ ...init, checkout: init.checkout }, out);
        if ("refused" in cloned) return { state: "refused", refused: cloned.refused, detail: cloned.detail };
        shadow = cloned;
      } else {
        try {
          shadow = await writeShadow(entry, init, out, env, options.home, ask);
        } catch (error) {
          if (ask === undefined) throw error;
          log(`the compiler engine refused ${init.service}'s GraphQL table; its GraphQL rows stay unarmed`, String(error));
          fs.rmSync(out, { recursive: true, force: true });
          shadow = await writeShadow(entry, init, out, env, options.home);
          graphqlReason = GRAPHQL_TABLE_UNUSABLE;
        }
        // Main's answer and its stamp, saved beside the shadow for a worktree's clone (carrick#5038). Never fails a start.
        try {
          writeShadowStamp(out, shadow, init.scan_root, heldBuildId());
        } catch {
          // No saved answer: a worktree clone is refused until the next start.
        }
      }
      if (ask !== undefined && shadow.graphql === undefined) graphqlReason = GRAPHQL_TABLE_UNUSABLE;
      if (shadow.native_tsc === null) return { state: "no_compiler", reason: "only TypeScript 6 is installed" };
      if (!sameFitOptions(shadow.options.service, shadow.options.judge)) {
        writePlaced(placedPath(heldRootOf(init, options.root), init.service, options.home), init.service, [], allUnarmed(mine, () => "options_differ"));
        return { engine: new IdleService() };
      }
      const message = shadow.message ?? null;
      const blocked = (row: ContractRow): string | null =>
        isGraphql(row) && graphqlReason !== null ? graphqlReason : isMessage(row) && message === null ? MESSAGE_RULES_MISSING : null;
      const armable = (row: ContractRow): boolean => hasValues(row) && blocked(row) === null;
      if (!mine.some(armable)) {
        writePlaced(placedPath(heldRootOf(init, options.root), init.service, options.home), init.service, [], allUnarmed(mine, (row) => (hasValues(row) ? blocked(row)! : "no_values")));
        return { engine: new IdleService() };
      }
      const w = path.dirname(shadow.tsconfig);
      const engine = (options.makeEngine ?? ((command, cwd) => new Ts7Lsp(command, cwd)))(shadow.native_tsc, w);
      const wire = checkWire(shadow.wire, shadow.tolerances.declarations);
      const answered = graphqlReason === null ? shadow.graphql : undefined;
      const gql: GraphqlChecks = {
        wire: answered ? [...wire, ...answered.lines, ...(answered.types_decl === null ? [] : [answered.types_decl])] : wire,
        leaves: new Map(Object.entries(answered?.leaves ?? {}).map(([root, leaf]) => [root, JSON.stringify(leaf)])),
        reason: graphqlReason,
        input: graphqlInput(mine, table),
      };
      const service = new HeldService(init, deps, engine, w, shadow.own_files, wire, gql, message, snapshot);
      try {
        service.loadRows(mine);
        await service.gate(false);
        snapshot.clear();
      } catch (error) {
        await engine.close();
        return { state: "error", reason: String(error) };
      }
      return { engine: service };
    },
  };
  return {
    rowCount: runtime.rowCount,
    async start(init: ServiceInit, startOptions?: StartOptions): Promise<StartResult> {
      if (init.checkout === undefined) return runtime.start(init);
      // A worktree's clone lives only as long as its engine: removed when the start gives no engine, and when the engine closes.
      const out = shadowDir(heldRootOf(init, options.root), init.service, options.home);
      const drop = (): void => fs.rmSync(out, { recursive: true, force: true });
      let result: StartResult;
      try {
        result = await runtime.start(init, startOptions);
      } catch (error) {
        drop();
        throw error;
      }
      if (!("engine" in result)) {
        drop();
        return result;
      }
      const engine = result.engine;
      const close = engine.close.bind(engine);
      engine.close = async () => {
        try {
          await close();
        } finally {
          drop();
        }
      };
      return result;
    },
  };
}
