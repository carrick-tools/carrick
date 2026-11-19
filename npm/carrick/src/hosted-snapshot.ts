import fs from "node:fs";
import path from "node:path";
import { realPath } from "./fs-util.ts";
import { createLogger } from "./log.ts";
import type { Logger } from "./log.ts";

const defaultLog = createLogger("carrick-generation");

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
  /** The checkout this row describes. */
  path?: string;
  /** Empty where the hosted or held commits are not one line of history. */
  hosted_commit: string;
  hosted_scanned_at: string | null;
  copy_commit: string;
  copy_scanned_at: string | null;
  head_holds_hosted: boolean;
  fetched: boolean;
  behind: number | null;
  head_detached: boolean;
  on_default_branch?: boolean | null;
  /** Why this repo's copy did not move in the last run, or null. */
  kept_reason?: string | null;
  /**
   * The one sentence that says this repo's copy is kept back and what to do,
   * written by the Rust side (`kept_sentence` in `src/local_mode/hosted.rs`).
   * Every surface prints this and words nothing of its own.
   */
  kept_line?: string | null;
  /** The same fact as a clause for the end of an edit-time line. */
  kept_short?: string | null;
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

/**
 * The sentence for every repo whose copy the record says is kept back, in the
 * record's order: the one place the session start reads it from.
 */
export function keptLines(sync: SyncRecord): string[] {
  return sync.repos.flatMap((repo) => (repo.kept_line ? [repo.kept_line] : []));
}

/** One repo of this machine's copy: its folder, the single commit it was indexed at (null when it has none), and when. */
export type LocalStamp = { repo: string; commit: string | null; indexedAt?: string };

/** A repo whose fact file and local copy name different commits. */
export type GenerationGap = { repo: string; fact: string; local: string; localIndexedAt: string | null };

/**
 * The one answer to "do the fact lines and the re-check read the same
 * generation": the repos whose fact-file commit (`factCommits`, by repo name,
 * as `commitsOf` reads it; `factRepos` maps each name to its folder here)
 * differs from the commit of this machine's copy of the same folder. Two
 * stamps are compared for equality and nothing is re-derived from HEAD. A
 * repo either side names no single commit for is left out, with a logged
 * stop word, never guessed at.
 */
export function generationGap(
  factCommits: Readonly<Record<string, string>> | null,
  factRepos: ReadonlyArray<{ repo: string; path: string }>,
  local: readonly LocalStamp[],
  log: Logger = defaultLog,
): GenerationGap[] {
  const gaps: GenerationGap[] = [];
  for (const stamp of local) {
    const fact = factRepos.find((entry) => realPath(entry.path) === realPath(stamp.repo));
    if (!fact) continue;
    if (stamp.commit === null) {
      log("generation: left out, no single local commit:", stamp.repo);
      continue;
    }
    const commit = factCommits?.[fact.repo];
    if (typeof commit !== "string") {
      log("generation: left out, no single fact commit:", fact.repo);
      continue;
    }
    if (commit !== stamp.commit) gaps.push({ repo: fact.repo, fact: commit, local: stamp.commit, localIndexedAt: stamp.indexedAt ?? null });
  }
  return gaps;
}
