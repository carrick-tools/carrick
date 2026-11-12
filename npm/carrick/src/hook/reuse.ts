// What one session's hooks remember between calls: which files' uses lines it
// has been shown, which files the Read hook has asked about, and when it last
// edited each workspace.
//
// The store is a file per session under `~/.carrick/sessions`, outside every
// repository: a scratch file about one conversation is not a thing to put in a
// user's tree, and a workspace can hold several repos while a session is one.
// Nothing here ever throws at its caller — an edit and a stop must not fail
// because a state file could not be written.
//
// It no longer lists the functions a session added: the end-of-task reuse
// nudge that read that list is gone (carrick#3436). A file an older install
// wrote still loads; `readSession` keeps only the fields below.

import { snapshotRepos } from "../hosted-snapshot.ts";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import type { CheckResult } from "../contract.ts";
import { safeId } from "./session-key.ts";
import { linkedCheckoutTop } from "./worktree.ts";

/** A session's accumulated record. */
export type SessionRecord = {
  /**
   * `repo::file` for every file whose uses lines this session has been shown
   * (carrick#2067), so the post-edit hook prints them once per file.
   */
  told: string[];
  /**
   * `lead::repo::file::name` for every new function whose lead line this
   * session has been shown (carrick#3249): each function is told of once.
   */
  led: string[];
  /**
   * Absolute paths the Read hook has already asked about (carrick#2069): it
   * asks once per file per session, whatever the answer was.
   */
  read: string[];
  /**
   * Absolute path -> the mtime (epoch ms) the shell hook last saw on a source
   * file a command wrote. A later command names the file again and the hook
   * sends it to the edit flow only when this moved.
   */
  written: Record<string, number>;
  /**
   * Workspace root -> epoch ms of this session's newest recorded edit there.
   * `checked.ts` compares it with the last `carrick check --changed`.
   */
  edited: Record<string, number>;
  /**
   * Absolute path -> the workspace root it was edited in and epoch ms of its
   * newest edit: the files the Stop hook states the session's check for
   * (`sessionCoverage` in `stop-wait.ts`, carrick#3466).
   */
  files: Record<string, { root: string; at: number; checkout?: string }>;
  /**
   * Workspace root -> the newest edit there the Stop hook has already spoken
   * about (checked, or queued `check --changed` for): it speaks once per change.
   */
  stated: Record<string, number>;
  /**
   * Workspace root -> the newest edit there whose every file the per-file
   * re-checks answered, as Stop stated it. `checked.ts` reads it like a clean
   * `check --changed` marker.
   */
  covered: Record<string, number>;
  /**
   * `root::<sorted operations>` for every clean or not-checked line Stop has
   * printed: the same set of operations is named once per session.
   */
  said: string[];
  /**
   * Fact-file row key -> when its line was delivered and the absolute paths
   * that line pointed at (`facts/lines.ts`): each row is said once per session,
   * claimed only once the line is printed.
   */
  facts: Record<string, { at: number; named: string[] }>;
  /** RFC 3339, when this file was last written. Drives the prune below. */
  updated: string;
};

/** Session files older than this are removed on the next write. */
const KEEP_DAYS = 7;

/** Where one session's record lives. */
export function sessionFile(sessionId: string, home: string = os.homedir()): string | null {
  const id = safeId(sessionId);
  return id === null ? null : path.join(home, ".carrick", "sessions", `${id}.json`);
}

/** The directory `carrick uninstall` deletes. */
export function sessionsDir(home: string = os.homedir()): string {
  return path.join(home, ".carrick", "sessions");
}

/** The `root -> epoch ms` entries of a record field that are numbers. */
function stamps(raw: unknown): Record<string, number> {
  if (typeof raw !== "object" || raw === null) return {};
  return Object.fromEntries(
    Object.entries(raw).filter(
      (entry): entry is [string, number] => typeof entry[1] === "number" && Number.isFinite(entry[1]),
    ),
  );
}

/** A session's record, or an empty one. Never throws. */
export function readSession(sessionId: string, home?: string): SessionRecord {
  const empty: SessionRecord = {
    told: [],
    led: [],
    read: [],
    written: {},
    edited: {},
    files: {},
    stated: {},
    covered: {},
    said: [],
    facts: {},
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
      told: Array.isArray(record.told)
        ? record.told.filter((entry): entry is string => typeof entry === "string")
        : [],
      led: Array.isArray(record.led) ? record.led.filter((entry): entry is string => typeof entry === "string") : [],
      read: Array.isArray(record.read)
        ? record.read.filter((entry): entry is string => typeof entry === "string")
        : [],
      written: stamps(record.written),
      edited: stamps(record.edited),
      files: Object.fromEntries(
        Object.entries(record.files ?? {}).filter(
          (entry): entry is [string, { root: string; at: number; checkout?: string }] =>
            typeof entry[1] === "object" &&
            entry[1] !== null &&
            typeof entry[1].root === "string" &&
            (entry[1].checkout === undefined || typeof entry[1].checkout === "string") &&
            typeof entry[1].at === "number" &&
            Number.isFinite(entry[1].at),
        ),
      ),
      stated: stamps(record.stated),
      covered: stamps(record.covered),
      said: Array.isArray(record.said) ? record.said.filter((entry): entry is string => typeof entry === "string") : [],
      facts: Object.fromEntries(
        Object.entries(record.facts ?? {}).filter(
          (entry): entry is [string, { at: number; named: string[] }] =>
            typeof entry[1] === "object" &&
            entry[1] !== null &&
            typeof entry[1].at === "number" &&
            Array.isArray(entry[1].named) &&
            entry[1].named.every((named) => typeof named === "string"),
        ),
      ),
      updated: typeof record.updated === "string" ? record.updated : "",
    };
  } catch {
    // A half-written file is a file to replace, not a reason to stop: nobody
    // edits this by hand and nothing downstream depends on its history.
    return empty;
  }
}

/**
 * Write `body` to `file` complete or not at all, creating the directory.
 * Returns whether it landed; never throws.
 */
export function writeAtomic(file: string, body: string): boolean {
  // Written beside the record and renamed over it: Claude Code runs Reads in
  // parallel, so several hook processes read, change and write one file at
  // once, and a writer that empties the file first lets a reader parse an
  // empty record and write that back over `found` and `told` (carrick#2069).
  // Two complete writes can still overwrite each other, which costs one
  // repeated print or one repeated check.
  const temp = `${file}.${process.pid}.tmp`;
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    fs.writeFileSync(temp, body, { mode: 0o600 });
    fs.renameSync(temp, file);
    return true;
  } catch {
    // Nothing to do and nothing to say: this record is a nicety and the edit
    // it rides behind is not.
    try {
      fs.unlinkSync(temp);
    } catch {
      // No temp file was left.
    }
    return false;
  }
}

function write(sessionId: string, record: SessionRecord, home?: string): void {
  const file = sessionFile(sessionId, home);
  if (file === null) return;
  writeAtomic(file, `${JSON.stringify(record, null, 2)}\n`);
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
    // Only `<id>.json`: the flat `<id>.recheck.<seq>.*` files beside it
    // belong to one record and are not records of their own.
    return fs
      .readdirSync(sessionsDir(home))
      .filter((name) => name.endsWith(".json") && !name.includes(".recheck.")).length;
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
 * The key a file's uses lines are remembered under: its repo and its
 * repo-relative path, so one path in two repos is two files.
 */
export function toldKey(result: CheckResult, fallbackFile: string): string {
  return `${result.repo ?? ""}::${result.file ?? fallbackFile}`;
}

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

/** Whether this session has already been shown the lead line under `key`. */
export function wasLed(sessionId: string, key: string, home?: string): boolean {
  return readSession(sessionId, home).led.includes(key);
}

/** Remember that this session has been shown the lead lines under `keys`. */
export function markLed(sessionId: string, keys: string[], home?: string): void {
  if (keys.length === 0) return;
  const current = readSession(sessionId, home);
  current.led = [...new Set([...current.led, ...keys])];
  current.updated = new Date().toISOString();
  write(sessionId, current, home);
}

/** The fact-row keys this session has already been given a line for. */
export function factsSaid(sessionId: string, home?: string): Set<string> {
  return new Set(Object.keys(readSession(sessionId, home).facts));
}

/** Remember that this session has been given the fact lines under `claims`. */
export function markFacts(sessionId: string, claims: Record<string, { at: number; named: string[] }>, home?: string): void {
  if (Object.keys(claims).length === 0) return;
  const current = readSession(sessionId, home);
  Object.assign(current.facts, claims);
  current.updated = new Date().toISOString();
  write(sessionId, current, home);
}

/**
 * Remember that this session edited `files` (absolute paths) in the workspace
 * at `root` at `at` (epoch ms).
 */
export function recordEdit(sessionId: string, root: string, at: number, home?: string, files: string[] = []): void {
  const current = readSession(sessionId, home);
  current.edited[root] = Math.max(at, current.edited[root] ?? 0);
  for (const file of files) {
    // The linked worktree top is recorded now: once it is removed, nothing
    // can recover it from the path (carrick#3595).
    const checkout = linkedCheckoutTop(file) ?? current.files[file]?.checkout;
    current.files[file] = {
      root,
      at: Math.max(at, current.files[file]?.at ?? 0),
      ...(checkout === undefined ? {} : { checkout }),
    };
  }
  current.updated = new Date().toISOString();
  write(sessionId, current, home);
}

/**
 * Remember that Stop spoke about the change in `root` up to the edit at
 * `upTo`, and whether every file of it was answered (`covered`).
 */
export function markStated(sessionId: string, root: string, upTo: number, covered: boolean, home?: string): void {
  const current = readSession(sessionId, home);
  current.stated[root] = Math.max(upTo, current.stated[root] ?? 0);
  if (covered) current.covered[root] = Math.max(upTo, current.covered[root] ?? 0);
  current.updated = new Date().toISOString();
  write(sessionId, current, home);
}

/** Whether Stop has already printed a clean or not-checked line for `key` in `root`. */
export function wasSaid(sessionId: string, root: string, key: string, home?: string): boolean {
  return readSession(sessionId, home).said.includes(`${root}::${key}`);
}

/** Remember that Stop printed the line for the operations under `key` in `root`. */
export function markSaid(sessionId: string, root: string, key: string, home?: string): void {
  const current = readSession(sessionId, home);
  current.said.push(`${root}::${key}`);
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

/** The mtime the shell hook last recorded for this absolute path, or undefined. */
export function writtenMtime(sessionId: string, file: string, home?: string): number | undefined {
  return readSession(sessionId, home).written[file];
}

/** Remember the mtime of each source file a shell command wrote. */
export function markWritten(sessionId: string, mtimes: Record<string, number>, home?: string): void {
  const current = readSession(sessionId, home);
  Object.assign(current.written, mtimes);
  current.updated = new Date().toISOString();
  write(sessionId, current, home);
}

/**
 * The one project slug the workspace's cached hosted snapshot names, or ""
 * when there is none or more than one (a tool call needs one project).
 * `snapshot.json` is written by the hosted refresh: `resolution.repos[]`
 * carries each connected repo's `project_slug`.
 */
export function snapshotProjectSlug(root: string): string {
  const repos = snapshotRepos(root);
  if (repos === null) return "";
  const slugs = new Set<string>();
  for (const repo of repos) {
    const slug = (repo as { project_slug?: unknown } | null)?.project_slug;
    if (typeof slug === "string" && slug !== "") slugs.add(slug);
  }
  return slugs.size === 1 ? [...slugs][0]! : "";
}
