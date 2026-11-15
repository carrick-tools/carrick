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
import { diffAgainst, textAtCommit } from "../facts/diff.ts";
import { contractKey } from "../facts/lines.ts";
import { commitsOf, indexedCommit } from "../facts/peers.ts";
import { factsPath, openFacts, type ContractRow } from "../facts/store.ts";
import { createLogger } from "../log.ts";
import type { HeldLine, HeldRequest, ServiceEngine, ServiceInit, ServiceRuntime, StartResult } from "./daemon.ts";
import { Ts7Lsp, type Diag as LspDiag, type Engine } from "./engine.ts";
import { placedPath, shadowDir } from "./paths.ts";
import {
  arm,
  attribute,
  checkWire,
  overlay,
  regionOfEdit,
  regionsFromHunks,
  resolveValue,
  type CheckInput,
  type Diag,
  type Overlay,
  type Region,
  type SiteFinder,
  type Value,
} from "./place.ts";
import { engineEntry, engineSiteFinder, writeShadow } from "./shadow.ts";

const log = createLogger("carrick-held-runtime");

/** The longest each git call of the gate may take (a slower one is `index_text_unavailable`). */
const GIT_BUDGET_MS = 2000;

export type RuntimeDeps = {
  root: string;
  home?: string;
  env?: NodeJS.ProcessEnv;
  /** The built compiler engine's entry; found from the environment when unset. */
  entry?: string | null;
  /** The producer response rows now. Default: the fact file's. */
  rows?: () => Promise<ContractRow[]>;
  /** Each repo's index commit, as the fact file's header names it. Default: the fact file's. */
  commits?: () => Promise<Record<string, string> | null>;
  /** Changes when the rows may have: the fact file's size and mtime by default. */
  stamp?: () => string;
  /** The held compiler over a shadow's `w` directory. */
  makeEngine?: (command: string, cwd: string) => Engine;
  /** The compiler engine's site finder; the engine's `find_site` action by default. */
  site?: SiteFinder;
};

type Unarmed = { label: string; file: string; reason: string; group: number; position: number | null };

const identity = (row: ContractRow): string => `${row.repo}\0${row.file}\0${row.label}\0${row.site ?? ""}`;
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

function readText(file: string): string | null {
  try {
    return fs.readFileSync(file, "utf8");
  } catch {
    return null;
  }
}

function writePlaced(file: string, service: string, armed: Array<{ label: string; file: string; group: number; position: number }>, unarmed: Unarmed[]): void {
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

/** Whether an unarmed reason may pass when the file is edited again. */
const transient = (reason: string): boolean => reason === "value_edited" || reason === "value_not_a_node" || reason === "not_wrappable" || reason.startsWith("clean_check_fails");

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
  private readonly armed = new Set<string>();
  private readonly unarmed = new Map<string, Unarmed>();
  private readonly views = new Map<string, View>();
  /** Files whose checks lost their place or may now be placed: gated again after the answer. */
  private readonly regate = new Set<string>();
  private stampSeen: string;
  private needsGate = false;

  private readonly init: ServiceInit;
  private readonly deps: Required<Pick<RuntimeDeps, "rows" | "stamp" | "commits" | "site">> & { root: string; home?: string; env: NodeJS.ProcessEnv };
  private readonly engine: Engine;
  private readonly w: string;
  private readonly wire: string[];

  constructor(
    init: ServiceInit,
    deps: Required<Pick<RuntimeDeps, "rows" | "stamp" | "commits" | "site">> & { root: string; home?: string; env: NodeJS.ProcessEnv },
    engine: Engine,
    w: string,
    ownFiles: string[],
    wire: string[],
  ) {
    this.init = init;
    this.deps = deps;
    this.engine = engine;
    this.w = w;
    this.wire = wire;
    this.ownFiles = new Set(ownFiles);
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
    for (const row of this.rows) {
      (row.values ?? []).forEach((v, at) => {
        const pos: Pos = { id: posId(row, at), row, at };
        live.add(pos.id);
        this.byFile.set(v.file, [...(this.byFile.get(v.file) ?? []), pos]);
      });
    }
    for (const id of [...this.values.keys()]) if (!live.has(id)) this.values.delete(id);
  }

  /** The text of `rel` at the index commit and the regions the disk text changed it by; or why neither is known. */
  private baseline(rel: string, text: string): { old: string; regions: Region[] } | "index_text_unavailable" {
    const commit = indexedCommit(this.commits, this.init.repo);
    if (commit === null) return "index_text_unavailable";
    const real = this.real(rel);
    const old = textAtCommit(real, commit, this.deps.env, GIT_BUDGET_MS);
    if (old === null) return "index_text_unavailable";
    if (old === text) return { old, regions: [] };
    const diff = diffAgainst(real, commit, this.deps.env, GIT_BUDGET_MS);
    if (diff === null || diff === "") return "index_text_unavailable";
    return { old, regions: regionsFromHunks(old, text, diff) };
  }

  private commits: Record<string, string> | null = null;

  /** D6: place every position of every file on the current text and arm what passes. `onlyNew`: leave decided checks alone. */
  async gate(onlyNew: boolean): Promise<void> {
    this.commits = await this.deps.commits();
    for (const rel of this.byFile.keys()) await this.gateFile(rel, onlyNew);
    for (const rel of this.byFile.keys()) this.setView(rel);
    for (const rel of [...this.views.keys()]) if (!this.byFile.has(rel)) this.views.delete(rel);
    this.writePlaced();
  }

  private async gateFile(rel: string, onlyNew: boolean): Promise<void> {
    const positions = this.byFile.get(rel) ?? [];
    const keysOf = (p: Pos): string[] => p.row.groups.map((_, g) => checkKey(p.row, g, p.at));
    const decided = (key: string): boolean => this.armed.has(key) || this.unarmed.has(key);
    if (onlyNew && positions.every((p) => keysOf(p).every(decided))) return;
    const no = (p: Pos, g: number, reason: string): void => {
      const key = checkKey(p.row, g, p.at);
      if (onlyNew && decided(key)) return;
      this.armed.delete(key);
      this.unarmed.set(key, { label: p.row.label, file: rel, reason, group: g, position: p.at });
    };
    if (!onlyNew) {
      for (const p of positions) for (const key of keysOf(p)) {
        this.armed.delete(key);
        this.unarmed.delete(key);
      }
    }
    const noneFor = (reason: string): void => {
      for (const p of positions) p.row.groups.forEach((_, g) => no(p, g, reason));
    };
    const text = readText(this.real(rel));
    if (text === null) return noneFor("file_unreadable");
    const base = this.baseline(rel, text);
    if (typeof base === "string") return noneFor(base);
    const entries: Entry[] = [];
    for (const p of positions) {
      const span = p.row.values![p.at]!.span;
      let placed: Value | string;
      try {
        placed = await resolveValue({ file: rel, start: span[0], end: span[1] }, base.regions, base.old, text, this.deps.site);
      } catch (error) {
        log("find_site failed", String(error));
        placed = "value_not_a_node";
      }
      if (typeof placed === "string") {
        p.row.groups.forEach((_, g) => no(p, g, placed as string));
        continue;
      }
      this.values.set(p.id, placed);
      entries.push({ pos: p, value: placed, groups: p.row.groups.map((_, g) => g) });
    }
    const shadow = this.shadow(rel);
    const inputs = this.inputsOf(entries);
    const clean = overlay(shadow, text, inputs, this.wire, "clean");
    for (const u of clean.unplaced) {
      const e = entries[u.n]!;
      e.groups.forEach((g) => no(e.pos, g, u.reason));
    }
    if (clean.checks.length > 0) {
      try {
        this.engine.set(shadow, clean.text);
        const cleanDiags = offsetDiags(clean.text, (await this.engine.pull([shadow])).get(shadow));
        const twin = overlay(shadow, text, inputs, this.wire, "twin");
        this.engine.set(shadow, twin.text);
        const twinDiags = offsetDiags(twin.text, (await this.engine.pull([shadow])).get(shadow));
        const verdict = arm(clean, cleanDiags, twinDiags);
        for (const k of verdict.armed) {
          const c = clean.checks.find((x) => x.k === k)!;
          const e = entries[c.n]!;
          const key = checkKey(e.pos.row, e.groups[c.group]!, e.pos.at);
          if (onlyNew && decided(key)) continue;
          this.armed.add(key);
          this.unarmed.delete(key);
        }
        for (const u of verdict.unarmed) {
          const e = entries[u.n]!;
          no(e.pos, e.groups[u.group]!, u.reason);
        }
      } finally {
        this.engine.set(shadow, text);
      }
    }
    this.known.set(rel, text);
  }

  /** The checks' inputs: one per entry, numbered by its place in `entries`. */
  private inputsOf(entries: Entry[]): CheckInput[] {
    return entries.map((e, n) => ({ n, value: e.value, groups: e.groups.map((g) => ({ type: e.pos.row.groups[g]!.type })) }));
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
    const ov = overlay(this.shadow(rel), text, inputs, this.wire, "clean");
    this.engine.set(this.shadow(rel), ov.text);
    this.views.set(rel, { ov, inputs, entries, text });
  }

  private writePlaced(): void {
    const armed: Array<{ label: string; file: string; group: number; position: number }> = [];
    const unarmed: Unarmed[] = [];
    for (const row of this.rows) {
      if (!hasValues(row)) {
        row.groups.forEach((_, g) => unarmed.push({ label: row.label, file: row.file, reason: "no_values", group: g, position: null }));
        continue;
      }
      row.values!.forEach((v, at) => {
        row.groups.forEach((_, g) => {
          const key = checkKey(row, g, at);
          if (this.armed.has(key)) armed.push({ label: row.label, file: v.file, group: g, position: at });
          else if (this.unarmed.has(key)) unarmed.push(this.unarmed.get(key)!);
        });
      });
    }
    writePlaced(placedPath(this.deps.root, this.init.service, this.deps.home), this.init.service, armed, unarmed);
  }

  /** Whether any position of `rel` is unarmed for a reason an edit can change. */
  private hasTransient(rel: string): boolean {
    return (this.byFile.get(rel) ?? []).some((p) => p.row.groups.some((_, g) => transient(this.unarmed.get(checkKey(p.row, g, p.at))?.reason ?? "")));
  }

  /** Move every position of `rel` with the edit from `old` to `text`; one that lost its place is dropped and the file is gated again. */
  private async remap(rel: string, old: string, text: string): Promise<void> {
    const regions = [regionOfEdit(old, text)];
    for (const pos of this.byFile.get(rel) ?? []) {
      const value = this.values.get(pos.id);
      if (value === undefined) continue;
      let moved: Value | string;
      try {
        moved = await resolveValue(value, regions, old, text, this.deps.site);
      } catch (error) {
        log("find_site failed", String(error));
        moved = "value_edited";
      }
      if (typeof moved === "string") {
        this.values.delete(pos.id);
        this.regate.add(rel);
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
      return;
    }
    this.ownFiles.add(rel);
    this.sigs.set(rel, this.sigOf(rel));
    const old = this.known.get(rel);
    if (old === text) return;
    fs.mkdirSync(path.dirname(this.shadow(rel)), { recursive: true });
    fs.writeFileSync(this.shadow(rel), text);
    this.known.set(rel, text);
    if (this.byFile.has(rel)) {
      if (old !== undefined) await this.remap(rel, old, text);
      if (this.hasTransient(rel)) this.regate.add(rel);
      this.setView(rel);
    } else this.engine.set(this.shadow(rel), text);
  }

  /**
   * D9: take what moved on disk behind the daemon, then the files this request
   * names. A moved file is opened in the engine, not left to the language
   * server's own disk watch: that watch lagged a fraction of a second behind in
   * measurement, which would answer with the file as it was.
   */
  private async sync(requested: string[]): Promise<void> {
    for (const rel of [...this.ownFiles]) {
      if (!requested.includes(rel) && this.sigOf(rel) !== this.sigs.get(rel)) await this.apply(rel);
    }
    for (const rel of requested) await this.apply(rel);
  }

  private async refreshRows(): Promise<void> {
    const stamp = this.deps.stamp();
    if (stamp === this.stampSeen) return;
    this.stampSeen = stamp;
    this.loadRows(await this.deps.rows());
    this.needsGate = true;
  }

  async answer(files: string[], request: HeldRequest): Promise<{ lines: HeldLine[]; after?: () => Promise<void> }> {
    await this.refreshRows();
    const requested = files
      .map((file) => path.relative(this.init.scan_root, file))
      .filter((rel) => rel !== "" && !rel.startsWith("..") && !path.isAbsolute(rel));
    await this.sync(requested);
    const lines = request.kind === "edit" ? await this.lines() : [];
    return { lines, after: () => this.afterAnswer() };
  }

  private async lines(): Promise<HeldLine[]> {
    const rels = [...this.views.keys()];
    if (rels.length === 0) return [];
    const pulled = await this.engine.pull(rels.map((rel) => this.shadow(rel)));
    const out: HeldLine[] = [];
    const said = new Set<string>();
    for (const rel of rels) {
      const view = this.views.get(rel)!;
      const at = lineMap(view.ov.text);
      const own = attribute(view.ov, offsetDiags(view.ov.text, pulled.get(this.shadow(rel))));
      // One line per (row, group), at the first position that broke.
      const broken = [...own.lines].map(([k, diags]) => ({ check: view.ov.checks.find((c) => c.k === k)!, diags })).sort((a, b) => a.diags[0]!.start - b.diags[0]!.start);
      for (const { check, diags } of broken) {
        const entry = view.entries[check.n]!;
        const g = entry.groups[check.group]!;
        const row = entry.pos.row;
        const key = `${contractKey(row, g)}\0${g}`;
        if (said.has(key)) continue;
        said.add(key);
        const first = diags[0]!;
        out.push({
          row_key: contractKey(row, g),
          group: g,
          label: row.label,
          consumer: [...new Set(row.groups[g]!.places.map((p) => p.service))].join(", "),
          file: rel,
          line: at.line(first.start),
          message: first.message,
          symbol: row.groups[g]!.symbol,
        });
      }
    }
    return out;
  }

  /** The twin pass, restored in a `finally`; then files that lost a check's place, then rows the fact file added or changed. */
  private async afterAnswer(): Promise<void> {
    for (const [rel, view] of this.views) {
      const shadow = this.shadow(rel);
      const twin = overlay(shadow, view.text, view.inputs, this.wire, "twin");
      try {
        this.engine.set(shadow, twin.text);
        const diags = offsetDiags(twin.text, (await this.engine.pull([shadow])).get(shadow));
        for (const lost of arm(view.ov, [], diags).unarmed) log(`twin_lost ${this.init.service} ${rel} ${lost.k}`);
      } finally {
        this.engine.set(shadow, view.ov.text);
      }
    }
    if (this.regate.size > 0) {
      const files = [...this.regate];
      this.regate.clear();
      this.commits = await this.deps.commits();
      for (const rel of files) {
        await this.gateFile(rel, false);
        this.setView(rel);
      }
      this.writePlaced();
    }
    if (this.needsGate) {
      this.needsGate = false;
      await this.gate(true);
    }
  }

  rss(): number | null {
    return this.engine.rss();
  }

  close(): Promise<void> {
    return this.engine.close();
  }
}

/** A service that holds no compiler: no route framework, or no row to check. */
class IdleService implements ServiceEngine {
  answer = async (): Promise<{ lines: HeldLine[] }> => ({ lines: [] });
  rss = (): number | null => null;
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

async function factCommits(root: string): Promise<Record<string, string> | null> {
  const store = await openFacts(root);
  if (store === null) return null;
  try {
    return commitsOf(store.meta);
  } finally {
    store.close();
  }
}

async function factRows(root: string): Promise<ContractRow[]> {
  const store = await openFacts(root);
  if (store === null) return [];
  try {
    return store.producerResponses();
  } finally {
    store.close();
  }
}

export function createRuntime(options: RuntimeDeps): ServiceRuntime {
  const env = options.env ?? process.env;
  const deps = {
    root: options.root,
    ...(options.home === undefined ? {} : { home: options.home }),
    rows: options.rows ?? (() => factRows(options.root)),
    stamp: options.stamp ?? (() => factStamp(options.root)),
    commits: options.commits ?? (() => factCommits(options.root)),
    env,
  };
  return {
    async start(init: ServiceInit): Promise<StartResult> {
      const entry = options.entry === undefined ? engineEntry(env) : options.entry;
      if (entry === null) return { state: "error", reason: "the compiler engine is not built" };
      const out = shadowDir(options.root, init.service, options.home);
      fs.rmSync(out, { recursive: true, force: true });
      const shadow = await writeShadow(entry, init, out, env, options.home);
      if (shadow.native_tsc === null) return { state: "no_compiler", reason: "only TypeScript 6 is installed" };
      const mine = (await deps.rows()).filter((row) => row.repo === init.repo && row.service === init.service && row.direction === "response");
      if (!mine.some((row) => (row.values?.length ?? 0) > 0)) {
        writePlaced(
          placedPath(options.root, init.service, options.home),
          init.service,
          [],
          mine.flatMap((row) => row.groups.map((_, g) => ({ label: row.label, file: row.file, reason: "no_values", group: g, position: null }))),
        );
        return { engine: new IdleService() };
      }
      const w = path.dirname(shadow.tsconfig);
      const engine = (options.makeEngine ?? ((command, cwd) => new Ts7Lsp(command, cwd)))(shadow.native_tsc, w);
      const site = options.site ?? engineSiteFinder(entry, env, options.home);
      const service = new HeldService(init, { ...deps, site }, engine, w, shadow.own_files, checkWire(shadow.wire, shadow.tolerances.declarations));
      try {
        service.loadRows(mine);
        await service.gate(false);
      } catch (error) {
        await engine.close();
        return { state: "error", reason: String(error) };
      }
      return { engine: service };
    },
  };
}
