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

/** This package's table layout; bumped with every table a new row kind adds. */
export const LOCAL_LAYOUT = 2;

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
 * schema this package reads (nothing is written then).
 */
export async function buildFacts(
  text: string,
  dest: string,
  local: LocalRepo[],
  fetchedAt: string,
): Promise<FactHeader | null> {
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
    writeFactsDatabase(sqlite, temp, header, lines, local, fetchedAt);
    fs.renameSync(temp, dest);
  } catch (error) {
    // No half-built file is left behind, whatever failed.
    fs.rmSync(temp, { force: true });
    throw error;
  }
  return header;
}

/** Write the fact database at `temp` from the header and NDJSON row lines. */
function writeFactsDatabase(sqlite: Sqlite, temp: string, header: FactHeader, lines: string[], local: LocalRepo[], fetchedAt: string): void {
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
    `);
    const meta = db.prepare("INSERT INTO meta (key, value) VALUES (?, ?)");
    const commits = Object.fromEntries((header.repos ?? []).map((repo) => [repo.repo, repo.commit]));
    for (const [key, value] of Object.entries({
      project: header.project_slug,
      digest: header.digest,
      schema: String(header.schema),
      writer: String(header.writer),
      built_at: header.built_at,
      fetched_at: fetchedAt,
      commits: JSON.stringify(commits),
    })) {
      meta.run(key, value);
    }
    const repo = db.prepare("INSERT INTO repos (repo, path) VALUES (?, ?)");
    for (const entry of local) repo.run(entry.repo, entry.path);
    const row = db.prepare(
      "INSERT INTO rows (side, protocol, label, service, repo, file, line, name, site, others, text) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    );
    const fn = db.prepare("INSERT INTO fns (repo, file, line, name, others, total, text) VALUES (?, ?, ?, ?, ?, ?, ?)");
    db.exec("BEGIN");
    for (const line of lines.slice(1)) {
      const raw = JSON.parse(line) as Record<string, unknown>;
      if (raw["k"] === "fn") {
        fn.run(
          String(raw["repo"]),
          String(raw["file"]),
          Number(raw["line"]),
          String(raw["name"]),
          JSON.stringify(Array.isArray(raw["others"]) ? raw["others"] : []),
          Number(raw["total"] ?? 0),
          String(raw["text"]),
        );
        continue;
      }
      // A row kind this package does not know is skipped (the wire adds kinds without a schema bump).
      if (raw["k"] !== "op") continue;
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
        JSON.stringify(Array.isArray(raw["others"]) ? raw["others"] : []),
        String(raw["text"]),
      );
    }
    db.exec("COMMIT");
    db.exec("CREATE INDEX rows_name ON rows (repo, name); CREATE INDEX rows_file ON rows (repo, file); CREATE INDEX fns_name ON fns (repo, name);");
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
