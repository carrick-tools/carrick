// The call sites a grep did not print, from the local index.
//
// A search for a function's name prints the files the search reached. The
// index already holds every caller of each function it knows (`uses` in
// `.carrick/index.json`), so a caller the output lacks is a site the agent
// has not seen. This says where they are, and nothing when the output already
// has them all: it never repeats what grep printed. It runs only after a
// grep or rg whose pattern names one identifier (`searchedNames`), asks no
// one, and costs $0.
//
// The index is read through a small name map cached beside the session
// records, keyed by the index file's mtime and size, so a grep after the first
// does not parse the whole index again.

import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { sessionsDir, markTold, wasTold, writeAtomic } from "./reuse.ts";

/** At most this many sites are named; the rest are counted. */
const MAX_SITES = 5;
/** At most this many names are answered for one command. */
const MAX_NAMES = 2;

type Site = { file: string; line: number };
type Definition = { file: string; line: number; callers: Site[]; total: number };
type NameMap = Record<string, Definition[]>;

/** The functions of at least five characters in the index, by name, with absolute paths. */
function buildMap(indexFile: string): NameMap {
  const index = JSON.parse(fs.readFileSync(indexFile, "utf8")) as {
    repos?: Array<{
      path?: string;
      uses?: Record<string, { functions?: Array<{ name?: string; line?: number; callers_total?: number; callers?: Array<{ file?: string; line?: number }> }> }>;
    }>;
  };
  const map: NameMap = {};
  for (const repo of index.repos ?? []) {
    if (typeof repo.path !== "string") continue;
    for (const [file, entry] of Object.entries(repo.uses ?? {})) {
      for (const fn of entry.functions ?? []) {
        if (typeof fn.name !== "string" || fn.name.length < 5 || typeof fn.line !== "number") continue;
        const callers = (fn.callers ?? [])
          .filter((caller): caller is { file: string; line: number } => typeof caller.file === "string" && typeof caller.line === "number")
          .map((caller) => ({ file: path.join(repo.path!, caller.file), line: caller.line }));
        (map[fn.name] ??= []).push({
          file: path.join(repo.path, file),
          line: fn.line,
          callers,
          total: typeof fn.callers_total === "number" ? fn.callers_total : callers.length,
        });
      }
    }
  }
  return map;
}

/** The name map for the index under `root`, from the cache when the index has not changed. Null when there is no index. */
export function nameMap(root: string, home: string = os.homedir()): NameMap | null {
  const indexFile = path.join(root, ".carrick", "index.json");
  let stamp: string;
  try {
    const stat = fs.statSync(indexFile);
    stamp = `${stat.mtimeMs}:${stat.size}`;
  } catch {
    return null;
  }
  const key = crypto.createHash("sha1").update(root).digest("hex").slice(0, 12);
  const cache = path.join(sessionsDir(home), `uses-${key}.map`);
  try {
    const cached = JSON.parse(fs.readFileSync(cache, "utf8")) as { stamp?: string; names?: NameMap };
    if (cached.stamp === stamp && cached.names) return cached.names;
  } catch {
    // No cache, or a half-written one: rebuild.
  }
  let names: NameMap;
  try {
    names = buildMap(indexFile);
  } catch {
    return null;
  }
  writeAtomic(cache, JSON.stringify({ stamp, names }));
  return names;
}

/** Whether `output` printed the file at `abs`: by its path from the root, from the cwd, or in full. */
function printed(output: string, abs: string, root: string, cwd: string, operands: Set<string>): boolean {
  if (operands.has(abs)) return true;
  const fromRoot = path.relative(root, abs);
  const fromCwd = path.relative(cwd, abs);
  return (
    output.includes(abs) ||
    (!fromRoot.startsWith("..") && output.includes(fromRoot)) ||
    (!fromCwd.startsWith("..") && output.includes(fromCwd))
  );
}

/**
 * One line per searched name whose index sites the grep output lacks, or null.
 * `output` is the command's stdout, `root` the workspace holding the index.
 */
export function missedSites(
  names: string[],
  operands: string[],
  output: string,
  root: string,
  cwd: string,
  session: string | undefined,
  home?: string,
): string | null {
  const map = nameMap(root, home);
  if (map === null) return null;
  const files = new Set(operands);
  const lines: string[] = [];
  for (const name of names) {
    if (lines.length >= MAX_NAMES) break;
    const definitions = map[name];
    // A name defined in more than one place is a word, not a function.
    if (!definitions || definitions.length !== 1) continue;
    const told = `grep:${name}`;
    if (session && wasTold(session, told, home)) continue;
    const definition = definitions[0]!;
    const rel = (file: string) => path.relative(root, file);
    const missed = definition.callers.filter(
      (caller) => caller.file !== definition.file && !printed(output, caller.file, root, cwd, files),
    );
    const definitionMissed = !printed(output, definition.file, root, cwd, files);
    if (missed.length === 0 && !definitionMissed) continue;
    const where = `${rel(definition.file)}:${definition.line}`;
    if (missed.length === 0) {
      lines.push(`Carrick: \`${name}\` is defined at ${where}, where this search did not reach.`);
    } else {
      const shown = missed.slice(0, MAX_SITES).map((site) => `${rel(site.file)}:${site.line}`);
      const more = missed.length - shown.length;
      lines.push(
        `Carrick: \`${name}\` (${where}) is also called where this search did not reach: ${shown.join(", ")}${more > 0 ? ` (+${more} more)` : ""}.`,
      );
    }
    if (session) markTold(session, [told], home);
  }
  return lines.length > 0 ? lines.join("\n") : null;
}
