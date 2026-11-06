import fs from "node:fs";
import path from "node:path";

/**
 * `resolution.repos` of the workspace's cached hosted snapshot
 * (`.carrick/hosted/snapshot.json`, written by the hosted refresh), or null
 * where the file is missing, unreadable, or holds no such array.
 */
export function snapshotRepos(root: string): unknown[] | null {
  try {
    const parsed: unknown = JSON.parse(
      fs.readFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), "utf8"),
    );
    const repos = (parsed as { resolution?: { repos?: unknown } } | null)?.resolution?.repos;
    return Array.isArray(repos) ? repos : null;
  } catch {
    return null;
  }
}

/**
 * One repo of the record below: the hosted index the cloud held for it at the
 * last check, the copy this machine compares against, and whether HEAD can
 * take the first (`SyncedRepo` in `src/local_mode/hosted.rs`).
 */
export type SyncedRepo = {
  repo: string;
  hosted_commit: string;
  hosted_scanned_at: string | null;
  copy_commit: string;
  copy_scanned_at: string | null;
  head_holds_hosted: boolean;
  fetched: boolean;
  behind: number | null;
  head_detached: boolean;
};

/** `.carrick/hosted/last-sync.json` (`carrick.hosted-sync/0`). */
export type SyncRecord = {
  schema: "carrick.hosted-sync/0";
  at: string;
  outcome: string;
  reason: string | null;
  repos: SyncedRepo[];
};

/**
 * The record the last `carrick refresh --hosted-only` the cloud answered
 * wrote (carrick#3432), or null where there is none or it is of another shape.
 * A few hundred bytes, so the session start reads it where it could not read
 * the snapshot beside it.
 */
export function lastSync(root: string): SyncRecord | null {
  const dir = path.join(root, ".carrick", "hosted");
  const file = path.join(dir, "last-sync.json");
  try {
    // A full `carrick refresh` rewrites the snapshot and not this record, so
    // a snapshot newer than the record means the copy it describes is gone.
    // The move writes the record after the snapshot, so its own run passes.
    const snapshot = fs.statSync(path.join(dir, "snapshot.json"), { throwIfNoEntry: false });
    if (snapshot && snapshot.mtimeMs > fs.statSync(file).mtimeMs) return null;
    const parsed = JSON.parse(fs.readFileSync(file, "utf8")) as Partial<SyncRecord> | null;
    if (
      parsed?.schema !== "carrick.hosted-sync/0" ||
      typeof parsed.at !== "string" ||
      !Array.isArray(parsed.repos)
    ) {
      return null;
    }
    const repos = parsed.repos.filter(
      (repo): repo is SyncedRepo =>
        typeof repo?.repo === "string" &&
        typeof repo.hosted_commit === "string" &&
        typeof repo.copy_commit === "string",
    );
    return { ...(parsed as SyncRecord), repos };
  } catch {
    return null;
  }
}
