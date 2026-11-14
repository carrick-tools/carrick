// The real `ServiceRuntime` of the held-compiler daemon (part 3, T5b).
//
// start:  write the service's shadow tree through the compiler engine, open a
//         TypeScript 7 language server over it, place every producer response
//         row's check on the current text and arm the ones that pass the gate.
// answer: sync files that changed on disk, put the edited file's text into the
//         engine, pull diagnostics of every check file and keep only a check's
//         own (R3). After the answer, the twin pass tests that each armed check
//         can still fail, and rows the fact file added or changed are gated.
//
// What an armed row IS is keyed by the row's identity plus its group's type
// text: a `ContractRow` has no id and a fact refresh may reorder rows.

import fs from "node:fs";
import path from "node:path";
import { contractKey } from "../facts/lines.ts";
import { factsPath, openFacts, type ContractRow } from "../facts/store.ts";
import { createLogger } from "../log.ts";
import type { HeldLine, HeldRequest, ServiceEngine, ServiceInit, ServiceRuntime, StartResult } from "./daemon.ts";
import { Ts7Lsp, type Diag as LspDiag, type Engine } from "./engine.ts";
import { placedPath, shadowDir } from "./paths.ts";
import { arm, attribute, overlay, type Diag, type Overlay, type PlaceRow } from "./place.ts";
import { engineEntry, writeShadow } from "./shadow.ts";

const log = createLogger("carrick-held-runtime");

export type RuntimeDeps = {
  root: string;
  home?: string;
  env?: NodeJS.ProcessEnv;
  /** The built compiler engine's entry; found from the environment when unset. */
  entry?: string | null;
  /** The producer response rows now. Default: the fact file's. */
  rows?: () => Promise<ContractRow[]>;
  /** Changes when the rows may have: the fact file's size and mtime by default. */
  stamp?: () => string;
  /** The held compiler over a shadow's `w` directory. */
  makeEngine?: (command: string, cwd: string) => Engine;
};

type Unarmed = { label: string; file: string; reason: string };

const identity = (row: ContractRow): string => `${row.repo}\0${row.file}\0${row.label}\0${row.site ?? ""}`;
const groupKey = (row: ContractRow, group: number): string => `${identity(row)}\0${row.groups[group]!.type}`;

/** A compiler's line/character diagnostics as offsets into the exact text that was set. */
function offsetDiags(text: string, diags: LspDiag[] | undefined): Diag[] {
  const starts = [0];
  for (let i = 0; i < text.length; i++) if (text[i] === "\n") starts.push(i + 1);
  return (diags ?? [])
    .filter((d) => d.severity === undefined || d.severity === 1)
    .map((d) => ({
      start: (starts[d.range.start.line] ?? text.length) + d.range.start.character,
      code: Number(d.code),
      message: d.message,
    }));
}

const lineOf = (text: string, offset: number): number => text.slice(0, offset).split("\n").length;

function readText(file: string): string | null {
  try {
    return fs.readFileSync(file, "utf8");
  } catch {
    return null;
  }
}

function writePlaced(file: string, service: string, armed: Array<{ label: string; file: string; group: number }>, unarmed: Unarmed[]): void {
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    fs.writeFileSync(file, JSON.stringify({ at: new Date().toISOString(), service, armed, unarmed }, null, 2));
  } catch (error) {
    log("could not write the placed record", String(error));
  }
}

/** The rows' check text for one file: only the groups in `only`, or every group. */
function placeRows(rows: ContractRow[], only: Set<string> | null): { rows: PlaceRow[]; origin: Array<{ row: ContractRow; groups: number[] }> } {
  const placed: PlaceRow[] = [];
  const origin: Array<{ row: ContractRow; groups: number[] }> = [];
  for (const row of rows) {
    const groups = row.groups.map((_, g) => g).filter((g) => only === null || only.has(groupKey(row, g)));
    if (groups.length === 0) continue;
    placed.push({
      id: origin.length,
      label: row.label,
      locator: row.locator,
      site: row.site,
      line: row.line,
      groups: groups.map((g) => ({ type: row.groups[g]!.type })),
    });
    origin.push({ row, groups });
  }
  return { rows: placed, origin };
}

type View = { ov: Overlay; rows: PlaceRow[]; origin: Array<{ row: ContractRow; groups: number[] }>; text: string };

/** One service's held compiler: its shadow, its checks and what is armed. */
class HeldService implements ServiceEngine {
  private readonly ownFiles: Set<string>;
  private readonly sigs = new Map<string, string>();
  /** Plain text of every file the engine holds open. */
  private readonly known = new Map<string, string>();
  private readonly byFile = new Map<string, ContractRow[]>();
  private readonly armed = new Set<string>();
  private readonly unarmed = new Map<string, Unarmed>();
  private readonly views = new Map<string, View>();
  private stampSeen: string;
  private needsGate = false;

  private readonly init: ServiceInit;
  private readonly deps: Required<Pick<RuntimeDeps, "rows" | "stamp">> & { root: string; home?: string };
  private readonly engine: Engine;
  private readonly w: string;

  constructor(
    init: ServiceInit,
    deps: Required<Pick<RuntimeDeps, "rows" | "stamp">> & { root: string; home?: string },
    engine: Engine,
    w: string,
    ownFiles: string[],
  ) {
    this.init = init;
    this.deps = deps;
    this.engine = engine;
    this.w = w;
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
    for (const row of rows) {
      if (row.repo !== this.init.repo || row.service !== this.init.service) continue;
      this.byFile.set(row.file, [...(this.byFile.get(row.file) ?? []), row]);
    }
  }

  /** D6: place every row of every file on the current text and arm what passes. `onlyNew`: leave decided groups alone. */
  async gate(onlyNew: boolean): Promise<void> {
    const decided = (key: string): boolean => this.armed.has(key) || this.unarmed.has(key);
    for (const [rel, rows] of this.byFile) {
      const keys = rows.flatMap((row) => row.groups.map((_, g) => groupKey(row, g)));
      if (onlyNew && keys.every(decided)) continue;
      const no = (row: ContractRow, g: number, reason: string): void => {
        const key = groupKey(row, g);
        if (onlyNew && decided(key)) return;
        this.unarmed.set(key, { label: row.label, file: row.file, reason });
      };
      const text = readText(this.real(rel));
      if (text === null) {
        for (const row of rows) row.groups.forEach((_, g) => no(row, g, "file_unreadable"));
        continue;
      }
      const shadow = this.shadow(rel);
      const all = placeRows(rows, null);
      const clean = overlay(shadow, text, all.rows, "clean");
      for (const u of clean.unplaced) {
        const row = all.origin[u.id]!.row;
        row.groups.forEach((_, g) => no(row, g, u.reason));
      }
      if (clean.checks.length > 0) {
        try {
          this.engine.set(shadow, clean.text);
          const cleanDiags = offsetDiags(clean.text, (await this.engine.pull([shadow])).get(shadow));
          const twin = overlay(shadow, text, all.rows, "twin");
          this.engine.set(shadow, twin.text);
          const twinDiags = offsetDiags(twin.text, (await this.engine.pull([shadow])).get(shadow));
          const verdict = arm(clean, cleanDiags, twinDiags);
          for (const k of verdict.armed) {
            const c = clean.checks.find((x) => x.k === k)!;
            const { row, groups } = all.origin[c.id]!;
            const g = groups[c.group]!;
            if (onlyNew && decided(groupKey(row, g))) continue;
            this.armed.add(groupKey(row, g));
            this.unarmed.delete(groupKey(row, g));
          }
          for (const u of verdict.unarmed) {
            const { row, groups } = all.origin[u.id]!;
            no(row, groups[u.group]!, u.reason);
          }
        } finally {
          this.engine.set(shadow, text);
        }
      }
      this.known.set(rel, text);
    }
    for (const rel of this.byFile.keys()) this.setView(rel);
    for (const rel of [...this.views.keys()]) if (!this.byFile.has(rel)) this.views.delete(rel);
    this.writePlaced();
  }

  /** Put `rel`'s text into the engine, with its armed checks wrapped when it has any. */
  private setView(rel: string): void {
    const text = this.known.get(rel);
    if (text === undefined) return;
    const rows = this.byFile.get(rel) ?? [];
    const view = placeRows(rows, this.armed);
    if (view.rows.length === 0) {
      this.views.delete(rel);
      this.engine.set(this.shadow(rel), text);
      return;
    }
    const ov = overlay(this.shadow(rel), text, view.rows, "clean");
    this.engine.set(this.shadow(rel), ov.text);
    this.views.set(rel, { ov, rows: view.rows, origin: view.origin, text });
  }

  private writePlaced(): void {
    const armed: Array<{ label: string; file: string; group: number }> = [];
    const unarmed: Unarmed[] = [];
    for (const rows of this.byFile.values()) {
      for (const row of rows) {
        row.groups.forEach((_, g) => {
          const key = groupKey(row, g);
          if (this.armed.has(key)) armed.push({ label: row.label, file: row.file, group: g });
          else if (this.unarmed.has(key)) unarmed.push(this.unarmed.get(key)!);
        });
      }
    }
    writePlaced(placedPath(this.deps.root, this.init.service, this.deps.home), this.init.service, armed, unarmed);
  }

  /** Make the shadow and the engine agree with the disk for `rel`. */
  private apply(rel: string): void {
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
    if (this.known.get(rel) === text) return;
    fs.mkdirSync(path.dirname(this.shadow(rel)), { recursive: true });
    fs.writeFileSync(this.shadow(rel), text);
    this.known.set(rel, text);
    if (this.byFile.has(rel)) this.setView(rel);
    else this.engine.set(this.shadow(rel), text);
  }

  /**
   * D9: take what moved on disk behind the daemon, then the files this request
   * names. A moved file is opened in the engine, not left to the language
   * server's own disk watch: that watch lagged a fraction of a second behind in
   * measurement, which would answer with the file as it was.
   */
  private sync(requested: string[]): void {
    for (const rel of [...this.ownFiles]) {
      if (!requested.includes(rel) && this.sigOf(rel) !== this.sigs.get(rel)) this.apply(rel);
    }
    for (const rel of requested) this.apply(rel);
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
    this.sync(requested);
    const lines = request.kind === "edit" ? await this.lines() : [];
    return { lines, after: () => this.afterAnswer() };
  }

  private async lines(): Promise<HeldLine[]> {
    const rels = [...this.views.keys()];
    if (rels.length === 0) return [];
    const pulled = await this.engine.pull(rels.map((rel) => this.shadow(rel)));
    const out: HeldLine[] = [];
    for (const rel of rels) {
      const view = this.views.get(rel)!;
      const own = attribute(view.ov, offsetDiags(view.ov.text, pulled.get(this.shadow(rel))));
      for (const [k, diags] of own.lines) {
        const check = view.ov.checks.find((c) => c.k === k)!;
        const { row, groups } = view.origin[check.id]!;
        const g = groups[check.group]!;
        const first = diags[0]!;
        out.push({
          row_key: contractKey(row, g),
          group: g,
          label: row.label,
          consumer: [...new Set(row.groups[g]!.places.map((p) => p.service))].join(", "),
          file: row.file,
          line: lineOf(view.ov.text, first.start),
          message: first.message,
        });
      }
    }
    return out;
  }

  /** The twin pass, restored in a `finally`; then rows the fact file added or changed. */
  private async afterAnswer(): Promise<void> {
    for (const [rel, view] of this.views) {
      const shadow = this.shadow(rel);
      const twin = overlay(shadow, view.text, view.rows, "twin");
      try {
        this.engine.set(shadow, twin.text);
        const diags = offsetDiags(twin.text, (await this.engine.pull([shadow])).get(shadow));
        for (const lost of arm(view.ov, [], diags).unarmed) log(`twin_lost ${this.init.service} ${rel} ${lost.k}`);
      } finally {
        this.engine.set(shadow, view.ov.text);
      }
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
  };
  return {
    async start(init: ServiceInit): Promise<StartResult> {
      const entry = options.entry === undefined ? engineEntry(env) : options.entry;
      if (entry === null) return { state: "error", reason: "the compiler engine is not built" };
      const out = shadowDir(options.root, init.service, options.home);
      fs.rmSync(out, { recursive: true, force: true });
      const shadow = await writeShadow(entry, init, out, env, options.home);
      if (shadow.native_tsc === null) return { state: "no_compiler", reason: "only TypeScript 6 is installed" };
      const mine = (await deps.rows()).filter((row) => row.repo === init.repo && row.service === init.service);
      if (!shadow.hono || mine.length === 0) {
        const reason = shadow.hono ? "no_site_found" : "no_hono";
        writePlaced(
          placedPath(options.root, init.service, options.home),
          init.service,
          [],
          mine.map((row) => ({ label: row.label, file: row.file, reason })),
        );
        return { engine: new IdleService() };
      }
      const w = path.dirname(shadow.tsconfig);
      const engine = (options.makeEngine ?? ((command, cwd) => new Ts7Lsp(command, cwd)))(shadow.native_tsc, w);
      const service = new HeldService(init, deps, engine, w, shadow.own_files);
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
