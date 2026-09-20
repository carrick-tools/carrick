// What one session has added that the index does not hold, and the one line
// the agent is told about it at the end of the task (carrick#1330).
//
// The measured problem: a reuse check that waits to be remembered does not
// run. Prose alone reached the index in none of five runs; a hook reached it
// in all five. So the check has to be delivered, not documented — but delivery
// costs a model turn, and most edits add no function, so it cannot ride the
// edit. The split is: the post-edit hook RECORDS silently (it already re-checks
// the edited file, and the list of new functions comes back in that answer),
// and the Stop hook SPEAKS once, with everything the task accumulated.
//
// The store is a file per session under `~/.carrick/sessions`, outside every
// repository: a scratch file about one conversation is not a thing to put in a
// user's tree, and a workspace can hold several repos while a session is one.
// Nothing here ever throws at its caller — an edit and a stop must not fail
// because a state file could not be written.
//
// Reference: `docs/reference/task-skills.md`, "The reuse nudge".

import fs from "node:fs";
import os from "node:os";
import path from "node:path";

/** One function the working tree declares and the index does not. */
export type NewFunction = {
  /** As the source spells it. */
  name: string;
  /** Repo-relative path of the file that declares it. */
  file: string;
  /** The commit the index this was compared against was taken at. */
  indexCommit: string;
};

/** A session's accumulated record. */
export type SessionRecord = {
  /** Every new function this session has seen, in the order it saw them. */
  found: NewFunction[];
  /** `file::name` for the ones a nudge has already listed. */
  nudged: string[];
  /** RFC 3339, when this file was last written. Drives the prune below. */
  updated: string;
};

/**
 * How many entries one nudge names.
 *
 * `find_similar` takes twenty, so a longer list would be cut by the agent
 * making the call. Cutting it here keeps the cut in one place and keeps the
 * line readable.
 */
export const MAX_ENTRIES = 20;

/** Session files older than this are removed on the next write. */
const KEEP_DAYS = 7;

/** A file name a session id can never escape from. */
function safeId(sessionId: string): string | null {
  return /^[A-Za-z0-9._-]{1,128}$/.test(sessionId) ? sessionId : null;
}

/** Where one session's record lives. */
export function sessionFile(sessionId: string, home: string = os.homedir()): string | null {
  const id = safeId(sessionId);
  return id === null ? null : path.join(home, ".carrick", "sessions", `${id}.json`);
}

/** The directory `carrick remove` deletes. */
export function sessionsDir(home: string = os.homedir()): string {
  return path.join(home, ".carrick", "sessions");
}

/** The key a `nudged` list holds an entry under. */
function key(entry: NewFunction): string {
  return `${entry.file}::${entry.name}`;
}

/** A session's record, or an empty one. Never throws. */
export function readSession(sessionId: string, home?: string): SessionRecord {
  const empty: SessionRecord = { found: [], nudged: [], updated: "" };
  const file = sessionFile(sessionId, home);
  if (file === null) return empty;
  let body: string;
  try {
    body = fs.readFileSync(file, "utf8");
  } catch {
    return empty;
  }
  try {
    const parsed: unknown = JSON.parse(body);
    if (typeof parsed !== "object" || parsed === null) return empty;
    const record = parsed as Partial<SessionRecord>;
    return {
      found: Array.isArray(record.found)
        ? record.found.filter(
            (entry): entry is NewFunction =>
              typeof entry?.name === "string" && typeof entry?.file === "string",
          )
        : [],
      nudged: Array.isArray(record.nudged)
        ? record.nudged.filter((entry): entry is string => typeof entry === "string")
        : [],
      updated: typeof record.updated === "string" ? record.updated : "",
    };
  } catch {
    // A half-written file is a file to replace, not a reason to stop: nobody
    // edits this by hand and nothing downstream depends on its history.
    return empty;
  }
}

function write(sessionId: string, record: SessionRecord, home?: string): void {
  const file = sessionFile(sessionId, home);
  if (file === null) return;
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    fs.writeFileSync(file, `${JSON.stringify(record, null, 2)}\n`, { mode: 0o600 });
  } catch {
    // Nothing to do and nothing to say: the nudge is a nicety and the edit it
    // rides behind is not.
  }
  prune(home);
}

/**
 * Drop session files nothing will read again.
 *
 * A session id is never reused, so every file here is dead the moment its
 * conversation ends and only the clock can say when that was. Without this the
 * directory grows one file per session forever.
 */
export function prune(home?: string, now: number = Date.now()): void {
  const dir = sessionsDir(home);
  let names: string[];
  try {
    names = fs.readdirSync(dir);
  } catch {
    return;
  }
  const cutoff = now - KEEP_DAYS * 24 * 60 * 60 * 1000;
  for (const name of names) {
    const file = path.join(dir, name);
    try {
      if (fs.statSync(file).mtimeMs < cutoff) fs.unlinkSync(file);
    } catch {
      // A file another process is removing at the same moment is already gone.
    }
  }
}

/**
 * Delete every session record on this machine, for `carrick remove`.
 *
 * Returns how many files went, so the command can stay silent about a machine
 * that never recorded one. The remover lives beside the writer, which is the
 * rule every other half of `carrick remove` follows (carrick#1034).
 */
export function removeSessions(home?: string): number {
  const dir = sessionsDir(home);
  let names: string[];
  try {
    names = fs.readdirSync(dir);
  } catch {
    return 0;
  }
  let removed = 0;
  for (const name of names) {
    try {
      fs.unlinkSync(path.join(dir, name));
      removed += 1;
    } catch {
      // Already gone, or not ours to remove.
    }
  }
  try {
    fs.rmdirSync(dir);
  } catch {
    // Something else is in it; the records are gone either way.
  }
  return removed;
}

/**
 * Add what one edit found, keeping the first sighting of each function.
 *
 * Deduplicated on file and name: an agent edits one file several times in a
 * task, and every one of those edits re-reports the same new function.
 */
export function record(sessionId: string, found: NewFunction[], home?: string): SessionRecord {
  const current = readSession(sessionId, home);
  const seen = new Set(current.found.map(key));
  for (const entry of found) {
    if (seen.has(key(entry))) continue;
    seen.add(key(entry));
    current.found.push(entry);
  }
  current.updated = new Date().toISOString();
  write(sessionId, current, home);
  return current;
}

/** What a nudge has not yet named, oldest first. */
export function pending(record: SessionRecord): NewFunction[] {
  const nudged = new Set(record.nudged);
  return record.found.filter((entry) => !nudged.has(key(entry)));
}

/** Mark entries as named, so the next stop of the same task says nothing. */
export function markNudged(sessionId: string, entries: NewFunction[], home?: string): void {
  const current = readSession(sessionId, home);
  const nudged = new Set(current.nudged);
  for (const entry of entries) nudged.add(key(entry));
  current.nudged = [...nudged];
  current.updated = new Date().toISOString();
  write(sessionId, current, home);
}

/**
 * The one line the Stop hook hands the model.
 *
 * Three things it has to carry, and each is load-bearing:
 *
 * * the names, so the call can be made without another read;
 * * `description` entries rather than `name`, because `find_similar` resolves
 *   a `name` against the index and these are exactly the functions it does not
 *   hold — a `name` entry for one of them comes back as an error;
 * * the two limits, because they decide what an empty answer means. The
 *   comparison is against the index's commit, which is this repo's default
 *   branch as the last scan saw it, so a function written earlier on this
 *   branch is listed here too. And the index holds no body hash: the match is
 *   on what a function is described as doing, not on its source.
 */
export function nudge(entries: NewFunction[]): string {
  const named = entries.slice(0, MAX_ENTRIES);
  const commits = [...new Set(named.map((entry) => entry.indexCommit).filter(Boolean))];
  const at =
    commits.length === 1
      ? `the index at ${commits[0]!.slice(0, 7)}`
      : "the index this workspace holds";
  const list = named
    .map((entry) => `${entry.name} (${entry.file})`)
    .join(", ");
  const more =
    entries.length > named.length ? ` ${entries.length - named.length} more are not listed.` : "";
  return [
    `This task added ${named.length} function${named.length === 1 ? "" : "s"} that ${at} does not hold: ${list}.${more}`,
    `Run the carrick-reuse skill now: one find_similar call, one \`description\` entry per function (a \`name\` entry resolves against the index, and these are not in it).`,
    `Two limits on the answer: the index was taken at ${commits.length === 1 ? commits[0]!.slice(0, 7) : "this workspace's last scan"} on the default branch, so anything added on this branch is compared against that; and the comparison is on what each function is described as doing, not on its source.`,
  ].join(" ");
}
