// The repo selection, written where every later command reads it.
//
// `carrick init` asks which repos in a folder this install covers
// (carrick#1338). The answer used to last exactly as long as the run: the
// proposal, the project and the connection were scoped to it, and then
// `carrick refresh`, `carrick index`, the editor hooks and the next `init` all
// went back to deriving every repo in the folder. So a repo somebody had said
// no to was scanned by the command that ran after the one they said it in
// (carrick#1344).
//
// `carrick-workspace.json` is where that answer belongs. The scanner already
// reads it (`src/local_mode/workspace.rs`): `exclude` names directories to
// leave out, `Workspace::load` honours it, and the read path behind
// `carrick check` asks the same question of the same file, so the folder's
// hooks stop answering for a repo that is not covered. It sits in the folder
// holding the repos, which is not itself a repository, so nothing tracked is
// touched.
//
// What this file adds is the record of who wrote what. The list is shared: a
// user can exclude a repo by hand, and `carrick remove` must not take that
// away with ours. So every name init adds is also recorded under `carrick`,
// and the remove path subtracts exactly that list and nothing else. The
// scanner declares the same key (`WrittenByInit`) so a round trip through its
// struct cannot drop it.

import fs from "node:fs";
import path from "node:path";

/** The file, in the folder that holds the repos. */
export const WORKSPACE_FILE = "carrick-workspace.json";

/** A parsed workspace file: every key kept, ours read out of it. */
type Document = Record<string, unknown>;

function parse(existing: string | null): Document {
  if (existing === null || existing.trim() === "") return {};
  const parsed: unknown = JSON.parse(existing);
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new Error(`${WORKSPACE_FILE} is not a JSON object`);
  }
  return parsed as Document;
}

function names(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((entry): entry is string => typeof entry === "string") : [];
}

/** The half of the document init owns, as it is written. */
function ours(document: Document): string[] {
  const section = document["carrick"];
  if (typeof section !== "object" || section === null) return [];
  return names((section as Document)["exclude"]);
}

function serialize(document: Document): string {
  return `${JSON.stringify(document, null, 2)}\n`;
}

/** The file as it is, or null when the folder has none. */
export function readWorkspaceFile(workspace: string): string | null {
  try {
    return fs.readFileSync(path.join(workspace, WORKSPACE_FILE), "utf8");
  } catch {
    return null;
  }
}

/**
 * The repos this folder's workspace file leaves out.
 *
 * Read by `carrick init` before it derives anything: the scanner has already
 * dropped an excluded repo by the time the proposal arrives, so this is the
 * only way the command can say which repo a `--repo` flag is asking for and
 * where the answer was written down. An unreadable or unparseable file
 * excludes nothing here; the scanner reports it.
 */
export function excludedRepos(workspace: string): string[] {
  const existing = readWorkspaceFile(workspace);
  if (existing === null) return [];
  try {
    return names(parse(existing)["exclude"]);
  } catch {
    return [];
  }
}

export type SelectionWrite = {
  /** The file body to write. */
  body: string;
  /** True when it differs from what was there. */
  changed: boolean;
  /** The names this call added, which is what `carrick remove` takes back. */
  added: string[];
};

/**
 * The document with these repos excluded, and the record of what we added.
 *
 * Idempotent: a name the file already excludes is left where it is and not
 * claimed, whether a user put it there or an earlier run did. Everything else
 * in the file — a `repos` list, a key this version knows nothing about, the
 * order they are in — is kept as it was found.
 *
 * Throws on a file that is not JSON, exactly as the hook writers do: a file
 * somebody hand-edited into something unparseable is reported, never
 * overwritten.
 */
export function withExclusions(existing: string | null, exclude: string[]): SelectionWrite {
  const document = parse(existing);
  const current = names(document["exclude"]);
  const added = exclude.filter((name) => !current.includes(name));
  const claimed = [...ours(document), ...added];
  const next: Document = { ...document, exclude: [...current, ...added] };
  if (claimed.length > 0) next["carrick"] = { exclude: claimed };
  const body = serialize(next);
  return { body, changed: body !== existing, added };
}

export type SelectionRemoval = {
  body: string;
  changed: boolean;
  /** The names taken back out. */
  removed: string[];
  /** True when nothing of anyone else's is left, so the file itself can go. */
  empty: boolean;
};

/**
 * The inverse: our exclusions out, everything else kept.
 *
 * `carrick remove` undoes an install, and an install is the only thing this
 * takes back. A name a user excluded by hand stays excluded, and a file that
 * holds their repo list stays a file — it was theirs before init ran. Only a
 * document that is nothing but what init put there is reported as one the
 * caller may delete.
 */
export function withoutOurExclusions(existing: string): SelectionRemoval {
  const document = parse(existing);
  const removed = ours(document);
  const kept = names(document["exclude"]).filter((name) => !removed.includes(name));
  const next: Document = {};
  for (const [key, value] of Object.entries(document)) {
    if (key === "carrick") continue;
    // An emptied list is dropped rather than left as `[]`: it is a key init
    // added, and a file left holding one reads as a decision somebody made.
    if (key === "exclude") {
      if (kept.length > 0) next["exclude"] = kept;
      continue;
    }
    next[key] = value;
  }
  // Nothing of anyone else's left: no keys at all, or an empty `repos` list
  // and nothing beside it.
  const left = Object.keys(next);
  const empty = left.length === 0 || (left.length === 1 && left[0] === "repos" && names(next["repos"]).length === 0);
  return { body: serialize(next), changed: serialize(next) !== existing, removed, empty };
}

/**
 * Write the selection into the folder, and say what was recorded.
 *
 * Null when there was nothing to exclude, which is the ordinary case: a single
 * repo is not a choice, and a folder whose repos are all covered has no
 * selection to persist and must not get a file it did not have.
 */
export function writeSelection(workspace: string, exclude: string[]): SelectionWrite | null {
  if (exclude.length === 0) return null;
  const target = path.join(workspace, WORKSPACE_FILE);
  const written = withExclusions(readWorkspaceFile(workspace), exclude);
  if (written.changed) fs.writeFileSync(target, written.body);
  return written;
}

/**
 * Take our exclusions back out of the folder's file. Null when there was none.
 *
 * The file goes with them when init is the reason it exists; a file holding a
 * user's own repo list or their own exclusions is rewritten without ours and
 * left where it is.
 */
export function removeSelection(workspace: string): { file: string; removed: string[]; deleted: boolean } | null {
  const existing = readWorkspaceFile(workspace);
  if (existing === null) return null;
  const cleaned = withoutOurExclusions(existing);
  if (cleaned.removed.length === 0) return null;
  const target = path.join(workspace, WORKSPACE_FILE);
  if (cleaned.empty) {
    fs.rmSync(target, { force: true });
    return { file: WORKSPACE_FILE, removed: cleaned.removed, deleted: true };
  }
  fs.writeFileSync(target, cleaned.body);
  return { file: WORKSPACE_FILE, removed: cleaned.removed, deleted: false };
}
