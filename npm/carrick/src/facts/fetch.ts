// The background refresh of the fact file. Never in a hook's path: a hook
// only starts it (`startFactsRefresh`), detached, and reads whatever file the
// last refresh left.
//
// One request to `POST /mcp/facts` with the stored CLI credential, sending the
// digest it already holds, so an unchanged project costs one read. A new
// digest answers with a presigned URL to gzipped NDJSON, which is fetched
// (no credential), built into SQLite under a temporary name and renamed
// (`store.ts`). `facts_building`, a 404 (route not deployed) and every other
// error mean no new facts: one log line, and the next trigger asks again.
// Wire contract: carrick-cloud `docs/internal/reference/edit-facts-wire.md`.

import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import { API_BASE, readCredential } from "../auth/credentials.ts";
import { createLogger } from "../log.ts";
import { hasMarker } from "../root.ts";
import { claimRefresh, cooldownMs, markerPath, scriptBeside, startDetached } from "../hook/refresh.ts";
import { snapshotProjectSlug } from "../hook/reuse.ts";
import { splitRemote } from "../init/repos.ts";
import { buildFacts, closeQuietly, FACTS_STALE_MS, factsPath, loadSqlite, SCHEMA, touchFetched } from "./store.ts";
import type { LocalRepo } from "./store.ts";

const log = createLogger("carrick-facts");

/** After `facts_building`, the next trigger may ask again this soon. */
export const FACTS_BUILDING_RETRY_MS = 60_000;
export const FACTS_MARKER = "last-facts-refresh";

export type RefreshOutcome = "unchanged" | "written" | "building" | "none";

export type FactsRefreshOptions = {
  env?: NodeJS.ProcessEnv;
  request?: typeof fetch;
  now?: number;
};

/** Each repo of the local index and where it is checked out (`.carrick/index.json`). */
export function localRepos(root: string): Array<{ name: string; path: string; remote: string | null }> {
  try {
    const index = JSON.parse(fs.readFileSync(path.join(root, ".carrick", "index.json"), "utf8")) as {
      repos?: Array<{ name?: unknown; path?: unknown; services?: Array<{ enrichment?: { remote?: unknown } }> }>;
    };
    return (index.repos ?? [])
      .filter((repo) => typeof repo.name === "string" && typeof repo.path === "string")
      .map((repo) => {
        const remote = (repo.services ?? []).map((service) => service.enrichment?.remote).find((value) => typeof value === "string");
        return { name: repo.name as string, path: repo.path as string, remote: (remote as string | undefined) ?? null };
      });
  } catch {
    return [];
  }
}

/**
 * The fact file's repos matched to their checkouts here. A header entry that
 * carries a remote (`owner/name`) maps only to the one checkout whose origin
 * names exactly that; one without maps only to the one checkout of exactly
 * its name. No match, or more than one, leaves the repo unmapped: its rows
 * are never read.
 */
export function matchRepos(repos: ReadonlyArray<{ repo: string; remote?: string | null }>, local: ReturnType<typeof localRepos>): LocalRepo[] {
  const out: LocalRepo[] = [];
  for (const { repo, remote } of repos) {
    const found =
      typeof remote === "string" && remote !== ""
        ? local.filter((entry) => entry.remote !== null && ownerName(entry.remote) === remote)
        : local.filter((entry) => entry.name === repo);
    if (found.length === 1) out.push({ repo, path: found[0]!.path });
  }
  return out;
}

/** The `owner/name` an origin names, in either spelling git accepts, or the text itself. */
function ownerName(remote: string): string {
  return splitRemote(remote)?.name ?? remote;
}

/** Whether a staged URL is https on an `amazonaws.com` host (a presigned S3 URL). */
function stagedUrlAllowed(url: string): boolean {
  try {
    const parsed = new URL(url);
    return parsed.protocol === "https:" && parsed.hostname.endsWith(".amazonaws.com");
  } catch {
    return false;
  }
}

async function knownDigest(root: string): Promise<string | null> {
  const file = factsPath(root);
  if (!fs.existsSync(file)) return null;
  const sqlite = await loadSqlite();
  if (!sqlite) return null;
  let db: { close: () => void } | null = null;
  try {
    const open = new sqlite.DatabaseSync(file, { readOnly: true });
    db = open;
    const row = open.prepare("SELECT value FROM meta WHERE key = 'digest'").get();
    return row ? String(row["value"]) : null;
  } catch {
    return null;
  } finally {
    closeQuietly(db);
  }
}

/** Ask the cloud for the project's fact file and bring the local one up to date. Never throws. */
export async function refreshFacts(root: string, options: FactsRefreshOptions = {}): Promise<RefreshOutcome> {
  const env = options.env ?? process.env;
  const request = options.request ?? fetch;
  const now = new Date(options.now ?? Date.now()).toISOString();
  try {
    const credential = readCredential(env);
    if (!credential) {
      log("not signed in; no fact file");
      return "none";
    }
    const project = snapshotProjectSlug(root);
    if (!project) {
      log("no single project for this workspace; no fact file");
      return "none";
    }
    const known = await knownDigest(root);
    const response = await request(`${API_BASE}/mcp/facts`, {
      method: "POST",
      redirect: "error",
      signal: AbortSignal.timeout(30_000),
      headers: { Authorization: `Bearer ${credential.token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ project, schema: SCHEMA, ...(known ? { known_digest: known } : {}) }),
    });
    if (response.status === 404) {
      log("facts route not found; no fact file");
      return "none";
    }
    let answer: Record<string, unknown>;
    try {
      answer = (await response.json()) as Record<string, unknown>;
    } catch {
      log(`facts answer not JSON (HTTP ${response.status})`);
      return "none";
    }
    const error = answer["error"] as { code?: unknown } | undefined;
    if (answer["success"] === false || !response.ok) {
      const code = typeof error?.code === "string" ? error.code : `HTTP ${response.status}`;
      log(`facts not available: ${code}`);
      return code === "facts_building" ? "building" : "none";
    }
    if (answer["unchanged"] === true) {
      await touchFetched(root, now);
      return "unchanged";
    }
    const url = answer["staged_url"];
    if (typeof url !== "string") {
      log("facts answer carries no staged_url");
      return "none";
    }
    // The staged file is fetched with no credential, so only from where the cloud stages it.
    if (!stagedUrlAllowed(url)) {
      log("facts staged_url is not an https amazonaws.com URL; not fetched");
      return "none";
    }
    const staged = await request(url, { redirect: "error", signal: AbortSignal.timeout(60_000) });
    if (!staged.ok) {
      log(`staged fact file not fetched (HTTP ${staged.status})`);
      return "none";
    }
    const bytes = Buffer.from(await staged.arrayBuffer());
    const gzipped = bytes.length >= 2 && bytes[0] === 0x1f && bytes[1] === 0x8b;
    const text = (gzipped ? zlib.gunzipSync(bytes) : bytes).toString("utf8");
    const header = JSON.parse(text.slice(0, text.indexOf("\n") >>> 0)) as { repos?: Array<{ repo?: unknown; remote?: unknown }> };
    const repos = (header.repos ?? [])
      .filter((entry): entry is { repo: string; remote?: unknown } => typeof entry.repo === "string")
      .map((entry) => ({ repo: entry.repo, remote: typeof entry.remote === "string" ? entry.remote : null }));
    const built = await buildFacts(text, factsPath(root), matchRepos(repos, localRepos(root)), now);
    return built ? "written" : "none";
  } catch (error) {
    log("fact refresh failed", String(error));
    return "none";
  }
}

/**
 * Start the refresh for `root` detached, at most once per `FACTS_STALE_MS`
 * across every hook and session. Returns whether it started. Never throws.
 */
export function startFactsRefresh(
  root: string,
  options: { env?: NodeJS.ProcessEnv; now?: number; start?: (command: string, args: string[], env: NodeJS.ProcessEnv) => void } = {},
): boolean {
  const env = options.env ?? process.env;
  if (env["CARRICK_NO_FACTS_REFRESH"]) return false;
  // A folder Carrick was never set up in gets nothing, not even a marker directory.
  if (!hasMarker(root)) return false;
  try {
    const cooldown = cooldownMs(env, "CARRICK_FACTS_COOLDOWN_MS", FACTS_STALE_MS);
    if (!claimRefresh(markerPath(root, FACTS_MARKER), options.now ?? Date.now(), cooldown)) return false;
    const { command, args } = scriptBeside(import.meta.url, "refresh-run", root);
    (options.start ?? startDetached)(command, args, { ...env, CARRICK_NO_KEEP_AWAKE: "1" });
    return true;
  } catch (error) {
    log("fact refresh not started", String(error));
    return false;
  }
}

/**
 * After `facts_building`, move the claim back so the next trigger, a minute
 * on, asks again rather than waiting out the whole cooldown.
 */
export function allowEarlyRetry(root: string, env: NodeJS.ProcessEnv = process.env, now: number = Date.now()): void {
  const cooldown = cooldownMs(env, "CARRICK_FACTS_COOLDOWN_MS", FACTS_STALE_MS);
  try {
    fs.writeFileSync(markerPath(root, FACTS_MARKER), `${new Date(now - cooldown + FACTS_BUILDING_RETRY_MS).toISOString()}\n`);
  } catch {
    // The next trigger after the full cooldown asks instead.
  }
}
