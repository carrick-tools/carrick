// The local fact file: a SQLite database built by the background refresh
// (`fetch.ts`) from the cloud's NDJSON and opened read-only by the hooks.
//
// Wire contract: carrick-cloud `docs/internal/reference/edit-facts-wire.md`
// (schema 1). The file lives at `<root>/.carrick/facts/edit-facts.s<schema>.l<LOCAL>.sqlite`
// and is written under a temporary name and renamed, so a hook never opens a
// half-built file. `LOCAL` is this package's table layout: a new row kind's
// table bumps it, so a file built by an older package is never found (and
// never read with a table missing); the refresh then fetches the whole file,
// as it does when there is none. A `meta` table holds the project, digest, schema, writer,
// built_at, fetched_at and each repo's commit; a file whose schema is not
// `SCHEMA` is not read.
//
// `node:sqlite` is loaded lazily and never throws at its caller: a Node that
// cannot load it gives no fact lines and one log line. Some Node 24 releases
// print an ExperimentalWarning when it loads; that one warning is dropped so a
// hook's stderr stays clean.

import fs from "node:fs";
import path from "node:path";
import { createLogger } from "../log.ts";
import { isRecord } from "../fs-util.ts";
import { snapshotProjectSlug } from "../hook/reuse.ts";

const log = createLogger("carrick-facts");

/** The fact-file schema this package reads. */
export const SCHEMA = 1;

/** A fact file fetched longer ago than this is refreshed by the next trigger. */
export const FACTS_STALE_MS = 10 * 60_000;

/** One row of the fact file (`k: "op"`), as the wire states it. */
export type FactRow = {
  side: "producer" | "consumer";
  protocol: string;
  label: string;
  service: string;
  repo: string;
  file: string;
  line: number;
  name: string | null;
  /** The operation string as written at the site, when the index holds it. */
  site: string | null;
  others: Array<{ service: string; repo: string; file: string; line: number }>;
  text: string;
};

/** A function's callers in other files (`k: "fn"`), as the wire states it. */
export type FnRow = {
  repo: string;
  file: string;
  line: number;
  name: string;
  others: Array<{ repo: string; file: string; line: number; name: string }>;
  total: number;
  text: string;
};

/** One declaration of a function name (`k: "name"`), as the wire states it. */
export type NameRow = { name: string; repo: string; file: string; line: number; text: string };

/** A package two or more services declare (`k: "pkg"`), as the wire states it. */
export type PkgRow = {
  package: string;
  /** `others_text`: the row's sentence without this service (absent in a file an older cloud wrote). */
  services: Array<{ repo: string; service: string; spec: string; manifest: string; others_text?: string }>;
  text: string;
};

/** One printed type of a contract row's other side (`k: "contract"`, group), as the wire states it. */
export type ContractGroup = {
  type: string;
  symbol: string | null;
  home: { file: string; line: number } | null;
  places: Array<{ service: string; repo: string; file: string; line: number | null; loc: string }>;
  total: number;
  shown: number;
  lead: string | null;
  more: string | null;
  tail: string | null;
  shortened: boolean;
};

/** A contract row (`k: "contract"`): the other end of an operation and its type. */
export type ContractRow = {
  side: "producer" | "consumer";
  protocol: string;
  label: string;
  direction: "request" | "response";
  service: string;
  repo: string;
  file: string;
  line: number | null;
  name: string | null;
  site: string | null;
  locator: "registration" | "call";
  groups: ContractGroup[];
  untyped: Array<{ service: string; repo: string; file: string; line: number | null; reason: string }>;
  total: number;
};

/**
 * One route a chain row reaches (`k: "chain"`, `routes[]`): its producer
 * contract row's site, how many calls away, the hops up to its handler, and
 * the line's words before the contract row's places (`lead`).
 */
export type ChainRoute = {
  protocol: string;
  side: "producer";
  label: string;
  service: string;
  repo: string;
  file: string;
  line: number | null;
  depth: 1 | 2;
  /** From the reached function's caller up to the handler, all in `repo`; `call` is where that hop calls the one below. */
  path: Array<{ file: string; line: number; name: string; call: number | null }>;
  lead: string;
};

/** A chain row (`k: "chain"`): a function the cloud walked to from routes' handlers. */
export type ChainRow = {
  repo: string;
  file: string;
  line: number;
  name: string;
  /** At most three, in the order the line tries them. */
  routes: ChainRoute[];
  /** Every route reached, kept or not. */
  total: number;
};

/** This package's table layout; bumped with every table a new row kind adds. */
export const LOCAL_LAYOUT = 3;

export function factsPath(root: string, schema: number = SCHEMA): string {
  return path.join(root, ".carrick", "facts", `edit-facts.s${schema}.l${LOCAL_LAYOUT}.sqlite`);
}

type Sqlite = typeof import("node:sqlite");
let loaded: Sqlite | null | undefined;

/** `node:sqlite`, or null where this Node cannot load it. */
export async function loadSqlite(): Promise<Sqlite | null> {
  if (loaded !== undefined) return loaded;
  const emit = process.emitWarning;
  process.emitWarning = function (warning: string | Error, ...rest: unknown[]) {
    const text = typeof warning === "string" ? warning : warning?.message;
    if (typeof text === "string" && text.startsWith("SQLite is an experimental feature")) return;
    return (emit as (...args: unknown[]) => void).call(process, warning, ...rest);
  } as typeof process.emitWarning;
  try {
    loaded = await import("node:sqlite");
  } catch (error) {
    log("node:sqlite did not load; no fact lines", String(error));
    loaded = null;
  } finally {
    process.emitWarning = emit;
  }
  return loaded;
}

/** A repo the fact file knows and where its main checkout sits on this machine. */
export type LocalRepo = { repo: string; path: string };

/** The read side the hooks use. */
export type FactStore = {
  meta: Record<string, string>;
  repos: LocalRepo[];
  byName: (repo: string, name: string) => FactRow[];
  unnamed: (repo: string, file: string) => FactRow[];
  /** The `fn` rows of a function `name` in `repo`, in any file. */
  callers: (repo: string, name: string) => FnRow[];
  /** Whether the file holds any `fn` row: a fact file without them cannot answer a Read. */
  hasCallers: () => boolean;
  /** Every declaration of a function `name`, in any repo, ordered by (repo, file). */
  declarations: (name: string) => NameRow[];
  /** Which of `names` are declared in some file but not in `file` of `repo`, in one query. */
  declaredOnlyElsewhere: (names: readonly string[], repo: string, file: string) => Set<string>;
  /** The `pkg` row of a package, or null. */
  packageRow: (name: string) => PkgRow | null;
  /**
   * The contract rows of `file` in `repo`, in wire order. `[]` for a file built
   * without the `contracts` table (an older file): its statement is prepared on
   * the first call, never in `openFacts`, so such a file opens as it always did.
   */
  contracts: (repo: string, file: string) => ContractRow[];
  /**
   * The chain rows of function `name` declared in `file` of `repo`, in wire
   * order. `[]` for a file built without the `chains` table, prepared on the
   * first call as `contracts` is.
   */
  chains: (repo: string, file: string, name: string) => ChainRow[];
  close: () => void;
};

function toRow(raw: Record<string, unknown>): FactRow {
  return {
    side: raw["side"] as FactRow["side"],
    protocol: String(raw["protocol"]),
    label: String(raw["label"]),
    service: String(raw["service"]),
    repo: String(raw["repo"]),
    file: String(raw["file"]),
    line: Number(raw["line"]),
    name: raw["name"] === null ? null : String(raw["name"]),
    site: raw["site"] === null || raw["site"] === undefined ? null : String(raw["site"]),
    others: JSON.parse(String(raw["others"])) as FactRow["others"],
    text: String(raw["text"]),
  };
}

/**
 * Open the fact file under `root` read-only, or null when there is none, it
 * cannot be read, or it carries another schema. Never throws.
 */
export async function openFacts(root: string): Promise<FactStore | null> {
  const file = factsPath(root);
  if (!fs.existsSync(file)) return null;
  const sqlite = await loadSqlite();
  if (!sqlite) return null;
  let db: InstanceType<Sqlite["DatabaseSync"]> | null = null;
  // Handed to the caller once the store is built; closed here on every other path.
  let handed = false;
  try {
    const open = new sqlite.DatabaseSync(file, { readOnly: true });
    db = open;
    const meta: Record<string, string> = {};
    for (const raw of open.prepare("SELECT key, value FROM meta").all()) meta[String(raw["key"])] = String(raw["value"]);
    if (meta["schema"] !== String(SCHEMA)) {
      log(`fact file has schema ${meta["schema"] ?? "none"}, this package reads ${SCHEMA}; not read`);
      return null;
    }
    // A file for another project (a workspace re-pointed since it was built) is not this one's.
    const project = snapshotProjectSlug(root);
    if (!project || meta["project"] !== project) {
      log(`fact file is for project ${meta["project"] ?? "none"}, this workspace is ${project || "unknown"}; not read`);
      return null;
    }
    const repos = open.prepare("SELECT repo, path FROM repos").all().map((raw) => ({ repo: String(raw["repo"]), path: String(raw["path"]) }));
    const byName = open.prepare("SELECT * FROM rows WHERE repo = ? AND name = ? ORDER BY id");
    const unnamed = open.prepare("SELECT * FROM rows WHERE repo = ? AND file = ? AND name IS NULL ORDER BY id");
    const callers = open.prepare("SELECT * FROM fns WHERE repo = ? AND name = ? ORDER BY id");
    const anyFn = open.prepare("SELECT 1 AS present FROM fns LIMIT 1");
    const declarations = open.prepare("SELECT * FROM names WHERE name = ? ORDER BY repo, file, line");
    const declaredOnlyElsewhere = open.prepare(
      "SELECT name FROM names WHERE name IN (SELECT value FROM json_each(?)) GROUP BY name HAVING SUM(repo = ? AND file = ?) = 0",
    );
    const packageRow = open.prepare("SELECT * FROM pkgs WHERE package = ? ORDER BY id LIMIT 1");
    let contracts: ReturnType<typeof open.prepare> | null | undefined;
    let chains: ReturnType<typeof open.prepare> | null | undefined;
    handed = true;
    return {
      meta,
      repos,
      byName: (repo, name) => byName.all(repo, name).map((raw) => toRow(raw as Record<string, unknown>)),
      unnamed: (repo, rel) => unnamed.all(repo, rel).map((raw) => toRow(raw as Record<string, unknown>)),
      callers: (repo, name) =>
        callers.all(repo, name).map((raw) => ({
          repo: String(raw["repo"]),
          file: String(raw["file"]),
          line: Number(raw["line"]),
          name: String(raw["name"]),
          others: JSON.parse(String(raw["others"])) as FnRow["others"],
          total: Number(raw["total"]),
          text: String(raw["text"]),
        })),
      hasCallers: () => anyFn.get() !== undefined,
      declaredOnlyElsewhere: (names, repo, rel) =>
        new Set(declaredOnlyElsewhere.all(JSON.stringify(names), repo, rel).map((raw) => String(raw["name"]))),
      declarations: (name) =>
        declarations.all(name).map((raw) => ({
          name: String(raw["name"]),
          repo: String(raw["repo"]),
          file: String(raw["file"]),
          line: Number(raw["line"]),
          text: String(raw["text"]),
        })),
      packageRow: (name) => {
        const raw = packageRow.get(name);
        if (!raw) return null;
        return { package: String(raw["package"]), services: JSON.parse(String(raw["services"])) as PkgRow["services"], text: String(raw["text"]) };
      },
      contracts: (repo, rel) => {
        if (contracts === undefined) {
          try {
            contracts = open.prepare("SELECT row FROM contracts WHERE repo = ? AND file = ? ORDER BY id");
          } catch {
            // No table: a file an older package built.
            contracts = null;
          }
        }
        if (contracts === null) return [];
        return contracts.all(repo, rel).map((raw) => JSON.parse(String(raw["row"])) as ContractRow);
      },
      chains: (repo, rel, name) => {
        if (chains === undefined) {
          try {
            chains = open.prepare("SELECT row FROM chains WHERE repo = ? AND file = ? AND name = ? ORDER BY id");
          } catch {
            // No table: a file an older package built.
            chains = null;
          }
        }
        if (chains === null) return [];
        return chains.all(repo, rel, name).map((raw) => JSON.parse(String(raw["row"])) as ChainRow);
      },
      close: () => open.close(),
    };
  } catch (error) {
    log("fact file not readable", String(error));
    return null;
  } finally {
    if (!handed) closeQuietly(db);
  }
}

/** Close a database handle, if open, without throwing. */
export function closeQuietly(db: { close: () => void } | null): void {
  try {
    db?.close();
  } catch {
    // Already closed.
  }
}

/** The header line of a fact file. */
export type FactHeader = {
  schema: number;
  writer: number;
  project_slug: string;
  digest: string;
  built_at: string;
  repos: Array<{ repo: string; service: string; commit: string }>;
};

/**
 * Build the fact file at `dest` from NDJSON `text` (header line, then rows),
 * written under a temporary name and renamed. `local` maps each repo to its
 * main checkout here. Returns the header, or null when the text is not a
 * schema this package reads (nothing is written then). `options.contract`:
 * the cloud's answer said this file carries contract rows; it is kept in `meta`.
 */
export async function buildFacts(text: string, dest: string, local: LocalRepo[], fetchedAt: string, options: { contract?: boolean } = {}): Promise<FactHeader | null> {
  const lines = text.split("\n").filter((line) => line.length > 0);
  if (lines.length === 0) return null;
  const header = JSON.parse(lines[0]!) as FactHeader;
  if (header.schema !== SCHEMA) {
    log(`fact file schema ${header.schema} is not ${SCHEMA}; not written`);
    return null;
  }
  const sqlite = await loadSqlite();
  if (!sqlite) return null;
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  const temp = `${dest}.${process.pid}.tmp`;
  fs.rmSync(temp, { force: true });
  try {
    writeFactsDatabase(sqlite, temp, header, lines, local, fetchedAt, options.contract === true);
    fs.renameSync(temp, dest);
  } catch (error) {
    // No half-built file is left behind, whatever failed.
    fs.rmSync(temp, { force: true });
    throw error;
  }
  return header;
}

/** The row kinds this package reads. */
const KNOWN_KINDS = new Set(["op", "fn", "name", "pkg", "contract", "chain"]);

const isText =(value: unknown): value is string => typeof value === "string" && value.length > 0;
const isLine = (value: unknown): boolean => value === null || (typeof value === "number" && Number.isInteger(value));
const isNameOrNull = (value: unknown): boolean => value === null || typeof value === "string";
const allOf = (value: unknown, each: (entry: Record<string, unknown>) => boolean): boolean =>
  Array.isArray(value) && value.every((entry) => isRecord(entry) && each(entry));
/** An op or fn row's `others`: whole entries, at least one (an empty list proves nothing; R36.6). */
const someOf = (value: unknown, each: (entry: Record<string, unknown>) => boolean): boolean =>
  Array.isArray(value) && value.length > 0 && allOf(value, each);

/**
 * Whether a wire row carries every field its kind requires, each of the
 * wire's type (carrick-cloud `docs/internal/reference/edit-facts-wire.md`):
 * a non-empty `text`, and every `others` or `services` entry whole. A row
 * that does not is skipped at ingest, so no line prints from a half row.
 */
function wholeRow(raw: Record<string, unknown>): boolean {
  switch (raw["k"]) {
    case "op":
      return (
        isText(raw["text"]) &&
        (raw["side"] === "producer" || raw["side"] === "consumer") &&
        isText(raw["protocol"]) &&
        isText(raw["label"]) &&
        isText(raw["service"]) &&
        isText(raw["repo"]) &&
        isText(raw["file"]) &&
        isLine(raw["line"]) &&
        isNameOrNull(raw["name"]) &&
        (raw["site"] === undefined || isNameOrNull(raw["site"])) &&
        someOf(raw["others"], (other) => isText(other["service"]) && isText(other["repo"]) && isText(other["file"]) && isLine(other["line"]))
      );
    // An fn or name row states a line, so it needs one (R33.7, F15).
    case "fn":
      return (
        isText(raw["text"]) &&
        isText(raw["repo"]) &&
        isText(raw["file"]) &&
        Number.isInteger(raw["line"]) &&
        Number.isInteger(raw["total"]) &&
        isText(raw["name"]) &&
        someOf(raw["others"], (other) => isText(other["repo"]) && isText(other["file"]) && isLine(other["line"]) && isNameOrNull(other["name"]))
      );
    case "name":
      return isText(raw["text"]) && isText(raw["name"]) && isText(raw["repo"]) && isText(raw["file"]) && Number.isInteger(raw["line"]);
    case "pkg":
      return (
        isText(raw["text"]) &&
        isText(raw["package"]) &&
        allOf(
          raw["services"],
          (entry) =>
            isText(entry["repo"]) &&
            isText(entry["service"]) &&
            typeof entry["spec"] === "string" &&
            isText(entry["manifest"]) &&
            (entry["others_text"] === undefined || isText(entry["others_text"])),
        )
      );
    // A contract row carries no top-level `text`: its words are in its groups.
    case "contract":
      return (
        (raw["side"] === "producer" || raw["side"] === "consumer") &&
        isText(raw["protocol"]) &&
        isText(raw["label"]) &&
        (raw["direction"] === "request" || raw["direction"] === "response") &&
        isText(raw["service"]) &&
        isText(raw["repo"]) &&
        isText(raw["file"]) &&
        isLine(raw["line"]) &&
        isNameOrNull(raw["name"]) &&
        (raw["site"] === undefined || isNameOrNull(raw["site"])) &&
        (raw["locator"] === "registration" || raw["locator"] === "call") &&
        Number.isInteger(raw["total"]) &&
        allOf(raw["untyped"], (entry) => isText(entry["repo"]) && isText(entry["file"]) && isLine(entry["line"])) &&
        Array.isArray(raw["groups"]) &&
        raw["groups"].length > 0 &&
        allOf(raw["groups"], (group) => {
          if (!isText(group["type"]) || !Number.isInteger(group["total"]) || !Number.isInteger(group["shown"])) return false;
          if (!allOf(group["places"], (place) => isText(place["repo"]) && isText(place["file"]) && isText(place["loc"]) && isLine(place["line"]))) return false;
          const places = (group["places"] as unknown[]).length;
          const shown = Number(group["shown"]);
          if (shown < 0 || shown > places) return false;
          // A printed group carries its whole sentence; a request group prints none.
          return shown === 0 || (typeof group["lead"] === "string" && typeof group["more"] === "string" && typeof group["tail"] === "string");
        })
      );
    // A chain row carries no top-level `text`: each kept route carries its own lead.
    case "chain":
      return (
        isText(raw["repo"]) &&
        isText(raw["file"]) &&
        isText(raw["name"]) &&
        Number.isInteger(raw["line"]) &&
        Array.isArray(raw["routes"]) &&
        raw["routes"].length > 0 &&
        Number.isInteger(raw["total"]) &&
        Number(raw["total"]) >= raw["routes"].length &&
        allOf(
          raw["routes"],
          (route) =>
            isText(route["label"]) &&
            isText(route["repo"]) &&
            isText(route["file"]) &&
            isText(route["lead"]) &&
            route["side"] === "producer" &&
            isLine(route["line"]) &&
            (route["depth"] === 1 || route["depth"] === 2) &&
            someOf(route["path"], (hop) => isText(hop["file"]) && isText(hop["name"]) && Number.isInteger(hop["line"]) && isLine(hop["call"])),
        )
      );
    default:
      return false;
  }
}

/** Write the fact database at `temp` from the header and NDJSON row lines. */
function writeFactsDatabase(sqlite: Sqlite, temp: string, header: FactHeader, lines: string[], local: LocalRepo[], fetchedAt: string, contract: boolean): void {
  const db = new sqlite.DatabaseSync(temp);
  try {
    db.exec(`
      CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
      CREATE TABLE repos (repo TEXT NOT NULL, path TEXT NOT NULL);
      CREATE TABLE rows (
        id INTEGER PRIMARY KEY, side TEXT, protocol TEXT, label TEXT, service TEXT, repo TEXT,
        file TEXT, line INTEGER, name TEXT, site TEXT, others TEXT, text TEXT
      );
      CREATE TABLE fns (
        id INTEGER PRIMARY KEY, repo TEXT, file TEXT, line INTEGER, name TEXT, others TEXT, total INTEGER, text TEXT
      );
      CREATE TABLE names (id INTEGER PRIMARY KEY, name TEXT, repo TEXT, file TEXT, line INTEGER, text TEXT);
      CREATE TABLE pkgs (id INTEGER PRIMARY KEY, package TEXT, services TEXT, text TEXT);
      CREATE TABLE contracts (
        id INTEGER PRIMARY KEY, repo TEXT, file TEXT, line INTEGER, side TEXT, protocol TEXT, label TEXT,
        direction TEXT, name TEXT, site TEXT, row TEXT
      );
      CREATE TABLE chains (id INTEGER PRIMARY KEY, repo TEXT, file TEXT, line INTEGER, name TEXT, row TEXT);
    `);
    const meta = db.prepare("INSERT INTO meta (key, value) VALUES (?, ?)");
    // Each repo's index commit, which the peer proof checks files against
    // (`peers.ts`). A repo whose services name different commits has none,
    // so its peers are never proven: which commit a peer's file was read at
    // is not known. Kept that strict because both live fact files measured
    // (2026-11-12) hold one commit per repo across all its services.
    const named = new Map<string, Set<unknown>>();
    for (const entry of header.repos ?? []) named.set(entry.repo, (named.get(entry.repo) ?? new Set()).add(entry.commit));
    const commits = Object.fromEntries([...named].filter(([, seen]) => seen.size === 1).map(([repo, seen]) => [repo, [...seen][0]]));
    for (const [key, value] of Object.entries({
      project: header.project_slug,
      digest: header.digest,
      schema: String(header.schema),
      writer: String(header.writer),
      built_at: header.built_at,
      fetched_at: fetchedAt,
      commits: JSON.stringify(commits),
      // Set when the cloud's answer said this file carries contract rows (`known_contract` on the next fetch).
      ...(contract ? { contract: "1" } : {}),
    })) {
      meta.run(key, value);
    }
    const repo = db.prepare("INSERT INTO repos (repo, path) VALUES (?, ?)");
    for (const entry of local) repo.run(entry.repo, entry.path);
    const row = db.prepare(
      "INSERT INTO rows (side, protocol, label, service, repo, file, line, name, site, others, text) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    );
    const fn = db.prepare("INSERT INTO fns (repo, file, line, name, others, total, text) VALUES (?, ?, ?, ?, ?, ?, ?)");
    const declared = db.prepare("INSERT INTO names (name, repo, file, line, text) VALUES (?, ?, ?, ?, ?)");
    const pkg = db.prepare("INSERT INTO pkgs (package, services, text) VALUES (?, ?, ?)");
    const contractRow = db.prepare(
      "INSERT INTO contracts (repo, file, line, side, protocol, label, direction, name, site, row) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    );
    const chainRow = db.prepare("INSERT INTO chains (repo, file, line, name, row) VALUES (?, ?, ?, ?, ?)");
    db.exec("BEGIN");
    let skipped = 0;
    for (const line of lines.slice(1)) {
      const parsed = JSON.parse(line) as unknown;
      if (!isRecord(parsed)) continue;
      const raw = parsed;
      // A row kind this package does not know is skipped (the wire adds kinds without a schema bump).
      if (!KNOWN_KINDS.has(String(raw["k"]))) continue;
      if (!wholeRow(raw)) {
        skipped += 1;
        continue;
      }
      if (raw["k"] === "contract") {
        contractRow.run(
          String(raw["repo"]),
          String(raw["file"]),
          Number(raw["line"]),
          String(raw["side"]),
          String(raw["protocol"]),
          String(raw["label"]),
          String(raw["direction"]),
          typeof raw["name"] === "string" ? raw["name"] : null,
          typeof raw["site"] === "string" ? raw["site"] : null,
          JSON.stringify(raw),
        );
        continue;
      }
      if (raw["k"] === "chain") {
        chainRow.run(String(raw["repo"]), String(raw["file"]), Number(raw["line"]), String(raw["name"]), JSON.stringify(raw));
        continue;
      }
      if (raw["k"] === "fn") {
        fn.run(
          String(raw["repo"]),
          String(raw["file"]),
          Number(raw["line"]),
          String(raw["name"]),
          JSON.stringify(raw["others"]),
          Number(raw["total"]),
          String(raw["text"]),
        );
        continue;
      }
      if (raw["k"] === "name") {
        declared.run(String(raw["name"]), String(raw["repo"]), String(raw["file"]), Number(raw["line"]), String(raw["text"]));
        continue;
      }
      if (raw["k"] === "pkg") {
        pkg.run(String(raw["package"]), JSON.stringify(raw["services"]), String(raw["text"]));
        continue;
      }
      row.run(
        String(raw["side"]),
        String(raw["protocol"]),
        String(raw["label"]),
        String(raw["service"]),
        String(raw["repo"]),
        String(raw["file"]),
        Number(raw["line"]),
        typeof raw["name"] === "string" ? raw["name"] : null,
        typeof raw["site"] === "string" ? raw["site"] : null,
        JSON.stringify(raw["others"]),
        String(raw["text"]),
      );
    }
    db.exec("COMMIT");
    if (skipped > 0) log(`${skipped} fact row(s) missing a field or with a mistyped one; skipped`);
    db.exec("CREATE INDEX rows_name ON rows (repo, name); CREATE INDEX rows_file ON rows (repo, file); CREATE INDEX fns_name ON fns (repo, name); CREATE INDEX names_name ON names (name); CREATE INDEX pkgs_package ON pkgs (package); CREATE INDEX contracts_file ON contracts (repo, file); CREATE INDEX chains_name ON chains (repo, file, name);");
  } finally {
    closeQuietly(db);
  }
}

/** Stamp `fetched_at` on an existing file (an unchanged answer). Never throws. */
export async function touchFetched(root: string, at: string): Promise<void> {
  const sqlite = await loadSqlite();
  if (!sqlite) return;
  let db: { close: () => void } | null = null;
  try {
    const open = new sqlite.DatabaseSync(factsPath(root));
    db = open;
    open.prepare("UPDATE meta SET value = ? WHERE key = 'fetched_at'").run(at);
  } catch (error) {
    log("could not stamp the fact file", String(error));
  } finally {
    closeQuietly(db);
  }
}
