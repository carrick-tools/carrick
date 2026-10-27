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
import type { CheckResult } from "../contract.ts";

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
  /**
   * `repo::file` for every file whose uses lines this session has been shown
   * (carrick#2067), so the post-edit hook prints them once per file.
   */
  told: string[];
  /**
   * Absolute paths the Read hook has already asked about (carrick#2069): it
   * asks once per file per session, whatever the answer was.
   */
  read: string[];
  /** RFC 3339, when this file was last written. Drives the prune below. */
  updated: string;
};

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

/** The directory `carrick uninstall` deletes. */
export function sessionsDir(home: string = os.homedir()): string {
  return path.join(home, ".carrick", "sessions");
}

/** The key a `nudged` list holds an entry under. */
function key(entry: NewFunction): string {
  return `${entry.file}::${entry.name}`;
}

/** A session's record, or an empty one. Never throws. */
export function readSession(sessionId: string, home?: string): SessionRecord {
  const empty: SessionRecord = {
    found: [],
    nudged: [],
    told: [],
    read: [],
    updated: "",
  };
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
      told: Array.isArray(record.told)
        ? record.told.filter((entry): entry is string => typeof entry === "string")
        : [],
      read: Array.isArray(record.read)
        ? record.read.filter((entry): entry is string => typeof entry === "string")
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
  // Written beside the record and renamed over it: Claude Code runs Reads in
  // parallel, so several hook processes read, change and write one file at
  // once, and a writer that empties the file first lets a reader parse an
  // empty record and write that back over `found` and `told` (carrick#2069).
  // Two complete writes can still overwrite each other, which costs one
  // repeated print or one repeated check.
  const temp = `${file}.${process.pid}.tmp`;
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    fs.writeFileSync(temp, `${JSON.stringify(record, null, 2)}\n`, { mode: 0o600 });
    fs.renameSync(temp, file);
  } catch {
    // Nothing to do and nothing to say: the nudge is a nicety and the edit it
    // rides behind is not.
    try {
      fs.unlinkSync(temp);
    } catch {
      // No temp file was left.
    }
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
 * How many session records this machine holds, changing nothing: the count
 * `carrick uninstall` lists before it asks (carrick#1573).
 */
export function sessionRecords(home?: string): number {
  try {
    return fs.readdirSync(sessionsDir(home)).length;
  } catch {
    return 0;
  }
}

/**
 * Delete every session record on this machine, for `carrick uninstall`.
 *
 * Returns how many files went, so the command can stay silent about a machine
 * that never recorded one. The remover lives beside the writer, which is the
 * rule every other half of `carrick uninstall` follows (carrick#1034).
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
 * The key a file's uses lines are remembered under: its repo and its
 * repo-relative path, so one path in two repos is two files.
 */
export function toldKey(result: CheckResult, fallbackFile: string): string {
  return `${result.repo ?? ""}::${result.file ?? fallbackFile}`;
}

/** The `told` key for the post-edit "not re-checked here" line, said once per session. */
export const NOTED_KEY = "carrick::noted-line";

/** Whether this session has already been shown the uses lines under `key`. */
export function wasTold(sessionId: string, key: string, home?: string): boolean {
  return readSession(sessionId, home).told.includes(key);
}

/** Remember that this session has been shown the uses lines under `keys`. */
export function markTold(sessionId: string, keys: string[], home?: string): void {
  if (keys.length === 0) return;
  const current = readSession(sessionId, home);
  const told = new Set(current.told);
  for (const key of keys) told.add(key);
  current.told = [...told];
  current.updated = new Date().toISOString();
  write(sessionId, current, home);
}

/** Whether the Read hook has already asked about this absolute path. */
export function wasRead(sessionId: string, file: string, home?: string): boolean {
  return readSession(sessionId, home).read.includes(file);
}

/** Remember that the Read hook has asked about this absolute path. */
export function markRead(sessionId: string, file: string, home?: string): void {
  const current = readSession(sessionId, home);
  if (current.read.includes(file)) return;
  current.read.push(file);
  current.updated = new Date().toISOString();
  write(sessionId, current, home);
}

/** What one delivery of the nudge said, for the hook that delivered it. */
export type Drained = {
  /** The line to hand the model. */
  line: string;
  /** How many functions it names. */
  named: number;
  /** How many the session has recorded in all, named or not. */
  found: number;
};

/**
 * The nudge for one session, marked as spoken, or nothing to say (carrick#1335).
 *
 * The one place the sequence lives, because two hooks deliver it: Claude Code's
 * `Stop`, and Codex's `UserPromptSubmit`, which is the only non-blocking
 * model-visible event Codex gives us. Both read the same store, speak the same
 * line and mark the same set; only the event name on the wire differs.
 *
 * The marking happens here, before the caller writes anything, and that order
 * is the point: a nudge that is written and not recorded fires again on the
 * next stop, and a task is several stops.
 */
export function drain(sessionId: string, home?: string): Drained | null {
  const current = readSession(sessionId, home);
  const waiting = pending(current);
  if (waiting.length === 0) return null;
  const line = nudge(waiting);
  markNudged(sessionId, waiting, home);
  return { line, named: waiting.length, found: current.found.length };
}

/**
 * The one line the Stop hook hands the model: how many functions, at most two
 * names, and the ask. Short on purpose, and it starts with "Carrick:" so the
 * user can tell where it came from. The two limits on the answer (what the
 * index was taken at, and that the match is on what a function is described as
 * doing) live in the `carrick-reuse` skill, where the answer is read.
 */
export function nudge(entries: NewFunction[]): string {
  const total = entries.length;
  const shown = entries.slice(0, 2).map((entry) => entry.name);
  const rest = total - shown.length;
  const list =
    rest > 0
      ? `${shown.join(", ")} and ${rest} more`
      : shown.length === 2
        ? `${shown[0]} and ${shown[1]}`
        : shown[0]!;
  return `Carrick: this task added ${total} new function${total === 1 ? "" : "s"} (${list}). Check them for existing copies with one find_similar call, then tell the user in one line what Carrick found.`;
}
