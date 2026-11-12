// A dependency an edit to a `package.json` added or re-specified, and what
// the other services that declare the package pin it at (`k: "pkg"` rows).
//
// Only a file that parses as JSON, now and rebuilt from before the edit,
// is read. A changed line counts when it is one `"<package>": "<spec>"`
// pair whose key sits directly in `dependencies`, `devDependencies`,
// `peerDependencies` or `optionalDependencies`, the parsed file agrees on
// that spec, and the file before the edit did not already hold it (a comma
// added to the line above a new last dependency changes nothing). The row
// prints unless every other service in it already has exactly the new spec,
// and only when each other service checked out here still holds its stated
// spec on disk (R33.4). A row that does not hold the edited manifest but
// lists another of its repo's, or whose entry for the edited manifest has no
// `others_text`, says nothing.
// `deno.json(c)` and import maps are a named gap.

import path from "node:path";
import { tokenize } from "../match.ts";
import type { FactStore } from "../store.ts";
import type { FileMemo } from "../files.ts";
import type { Candidate } from "../lines.ts";
import { textBefore } from "./same-name.ts";

const SECTIONS = new Set(["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"]);

/** One `"key": "value"` pair on a line, with the raw (still escaped) text of each. */
const PAIR = /^\s*"((?:[^"\\]|\\.)*)"\s*:\s*"((?:[^"\\]|\\.)*)"\s*,?\s*$/;

/** A `package.json` of the repo's own; one under `node_modules/` is an installed package's, never read. */
export function isManifest(file: string): boolean {
  return path.basename(file) === "package.json" && !file.split(/[\\/]/).includes("node_modules");
}

/**
 * For each 1-based line, the key paths of the object keys written on it
 * (`["dependencies", "react"]`), read from the JSON's tokens; null when the
 * text does not tokenize.
 */
function keysByLine(text: string): Map<number, Array<Array<string | null>>> | null {
  const tokens = tokenize(text);
  if (tokens === null) return null;
  const out = new Map<number, Array<Array<string | null>>>();
  // The key each open container was written under (null for the root and array items).
  const open: Array<string | null> = [];
  let pending: string | null = null;
  for (const [index, token] of tokens.entries()) {
    if (token.kind === "punc" && (token.value === "{" || token.value === "[")) {
      open.push(pending);
      pending = null;
    } else if (token.kind === "punc" && (token.value === "}" || token.value === "]")) {
      open.pop();
      pending = null;
    } else if (token.kind === "str" && tokens[index + 1]?.kind === "punc" && tokens[index + 1]!.value === ":") {
      const key = text.slice(token.start + 1, token.end - 1);
      out.set(token.line, [...(out.get(token.line) ?? []), [...open.slice(1), key]]);
      pending = key;
    } else if (token.kind === "punc" && token.value === ",") pending = null;
  }
  return out;
}

function parse(text: string): Record<string, unknown> | null {
  try {
    const value = JSON.parse(text) as unknown;
    return value !== null && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

function specIn(manifest: Record<string, unknown> | null, section: string, name: string): unknown {
  const deps = manifest?.[section];
  return deps !== null && typeof deps === "object" && Object.hasOwn(deps, name) ? (deps as Record<string, unknown>)[name] : undefined;
}

/** The `pkg` lines for one edited manifest. */
export function packageLines(
  store: FactStore,
  at: { repo: string; rel: string; top: string },
  where: ReadonlyMap<string, string>,
  text: string,
  change: { lines?: number[] | null; removed?: number[] | null; removedText?: string[] | null },
  memo: FileMemo,
): Candidate[] {
  if (!change.lines || change.lines.length === 0 || !change.removed || !change.removedText) return [];
  const now = parse(text);
  const old = textBefore(text, change.lines, change.removed, change.removedText);
  const then = old === null ? null : parse(old);
  if (now === null || then === null) return [];
  const keys = keysByLine(text);
  if (keys === null) return [];
  const fileLines = text.split("\n");
  const out: Candidate[] = [];
  for (const line of new Set(change.lines)) {
    const pair = PAIR.exec(fileLines[line - 1] ?? "");
    const written = keys.get(line);
    if (!pair || written?.length !== 1) continue;
    const [section, rawName, extra] = written[0]!;
    if (extra !== undefined || typeof section !== "string" || !SECTIONS.has(section) || rawName !== pair[1]) continue;
    let name: string;
    let spec: string;
    try {
      name = JSON.parse(`"${pair[1]}"`) as string;
      spec = JSON.parse(`"${pair[2]}"`) as string;
    } catch {
      continue;
    }
    // Already this spec before the edit, in any of the four sections: a move
    // between sections changes no version.
    if (specIn(now, section, name) !== spec || [...SECTIONS].some((any) => specIn(then, any, name) === spec)) continue;
    const row = store.packageRow(name);
    if (row === null) continue;
    const others = row.services.filter((service) => !(service.repo === at.repo && service.manifest === at.rel));
    if (others.length === 0 || others.every((service) => service.spec === spec)) continue;
    const owners = row.services.filter((service) => service.repo === at.repo && service.manifest === at.rel);
    // Two services declare the package in this very manifest: the sentence
    // for one names the other at the spec this edit just changed (R36.1).
    if (owners.length > 1) continue;
    const own = owners[0];
    // The edited manifest is not in the row, but the row lists its repo: one
    // of those entries may be this manifest under another path, so the
    // sentence could state its old spec (R33.4, F4). With its own entry
    // found, other entries of the same repo are checked on disk like any other.
    if (!own && others.some((service) => service.repo === at.repo)) continue;
    // The edited manifest's own entry carries the sentence without it, which
    // would otherwise state its old spec; an entry with none says nothing
    // (R33.4, F5). A manifest the row does not hold, in a repo it does not
    // list, prints the row's sentence.
    const text = own ? own.others_text : row.text;
    if (!text) continue;
    // A service of the edited repo is read in the edited manifest's checkout.
    const baseOf = (repo: string): string | undefined => (repo === at.repo ? at.top : where.get(repo));
    const named = others.flatMap((service) => {
      const base = baseOf(service.repo);
      return base ? [path.join(base, service.manifest)] : [];
    });
    // Each other service checked out here must still hold the stated spec on
    // disk (R33.1, R33.4); one in a repo not checked out is stated from the index.
    const holds = (): boolean =>
      others.every((service) => {
        const base = baseOf(service.repo);
        if (base === undefined) return true;
        const manifest = memo.text(path.join(base, service.manifest));
        const parsed = manifest === null ? null : parse(manifest);
        return parsed !== null && [...SECTIONS].some((section) => specIn(parsed, section, name) === service.spec);
      });
    out.push({ key: `pkg::${name}`, text, named, holds });
  }
  return out;
}
