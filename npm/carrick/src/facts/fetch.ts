// The background refresh of the fact file. Never in a hook's path: a hook
// only starts it (`startFactsRefresh`), detached, and reads whatever file the
// last refresh left.
//
// One request to `POST /mcp/facts` with the stored CLI credential, sending the
// digest and writer it already holds, so an unchanged project costs one read.
// A new digest or writer answers with a presigned URL to gzipped NDJSON, which
// is fetched (no credential, https amazonaws.com only), built into SQLite
// under a temporary name and renamed (`store.ts`). An error answer, a 404
// (route not deployed) and every other failure mean no new facts: one log
// line. Only an error marked `retriable: true` (a build in flight, whatever
// its code) moves the next ask a minute on (`runFactsRefresh`); the rest wait
// out the cooldown. The code is logged, never branched on: one code is sent
// both ways.
// Wire contract: carrick-cloud `docs/internal/reference/edit-facts-wire.md`.

import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import { API_BASE, readCredential } from "../auth/credentials.ts";
import { createLogger } from "../log.ts";
import { hasMarker } from "../root.ts";
import { claimRefresh, cooldownMs, forwardInBackground, markerPath, scriptBeside, startDetached } from "../hook/refresh.ts";
import type { RefreshOptions } from "../hook/refresh.ts";
import { status as readStatus } from "../cli.ts";
import type { StatusResult } from "../contract.ts";
import { snapshotProjectSlug } from "../hook/reuse.ts";
import { chainLinesEnabled, contractLinesEnabled, factsEnabled } from "./enabled.ts";
import { buildFacts, closeQuietly, FACTS_STALE_MS, factsPath, loadSqlite, SCHEMA, touchFetched } from "./store.ts";
import { localRepos, matchRepos } from "./repo-match.ts";

const log = createLogger("carrick-facts");

/** After a retriable error, the next trigger may ask again this soon. */
export const FACTS_BUILDING_RETRY_MS = 60_000;
/**
 * How long the next ask waits after an answer that is neither a file nor a
 * retriable error (no facts for this project, a refused request). The ordinary
 * cooldown is now a minute, so without this a workspace with no facts would
 * ask the cloud every minute.
 */
export const FACTS_FAILED_WAIT_MS = 10 * 60_000;
export const FACTS_MARKER = "last-facts-refresh";

/** `retry`: the cloud answered a retriable error (a build in flight). */
export type RefreshOutcome = "unchanged" | "written" | "retry" | "none";

export type FactsRefreshOptions = {
  env?: NodeJS.ProcessEnv;
  request?: typeof fetch;
  now?: number;
  /** Injected in tests: the workspace status and the detached start of the hosted move that a new fact file begins. */
  forward?: { status?: () => Promise<StatusResult | null>; start?: RefreshOptions["start"] };
};

/** Whether a staged URL is https on an `amazonaws.com` host (a presigned S3 URL). */
function stagedUrlAllowed(url: string): boolean {
  try {
    const parsed = new URL(url);
    return parsed.protocol === "https:" && parsed.hostname.endsWith(".amazonaws.com");
  } catch {
    return false;
  }
}

/**
 * The digest and writer of the local fact file, or null when there is no
 * readable file holding both. The cloud answers `unchanged` only when both
 * match, so a wording fix (a new writer) reaches this file without a new scan.
 *
 * A file for another project than `project` (a workspace moved since it was
 * fetched) is taken away here, in the one read: no read trusts it
 * (`openFacts`), and a digest it shared with the right project's file could
 * keep it for good. What the cloud then sends is the only file left.
 */
async function knownFile(root: string, env: NodeJS.ProcessEnv, project: string): Promise<{ digest: string; writer: number; contract: boolean } | null> {
  const file = factsPath(root);
  if (!fs.existsSync(file)) return null;
  const sqlite = await loadSqlite();
  if (!sqlite) return null;
  let db: { close: () => void } | null = null;
  let otherProject = false;
  try {
    const open = new sqlite.DatabaseSync(file, { readOnly: true });
    db = open;
    const meta = new Map(open.prepare("SELECT key, value FROM meta WHERE key IN ('digest', 'writer', 'contract', 'commits', 'project')").all().map((raw) => [String(raw["key"]), String(raw["value"])]));
    const held = meta.get("project");
    if (held !== undefined && held !== project) {
      otherProject = true;
      return null;
    }
    const digest = meta.get("digest");
    const writer = Number(meta.get("writer"));
    if (!digest || !Number.isInteger(writer)) return null;
    // With the contract line on, a file an older package built has no `contracts`
    // table to read from: no known digest, so the cloud sends the whole file.
    const hasTable = (name: string) => open.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?").get(name) !== undefined;
    if (contractLinesEnabled(env) && !hasTable("contracts")) return null;
    // Likewise with the chain line on and no `chains` table.
    if (chainLinesEnabled(env) && !hasTable("chains")) return null;
    // A checkout the index lists now that the file's repo table does not map (a folder
    // renamed from its repo, built before the origin was read; a clone made since): the
    // file is built again, whatever its digest.
    const stored = open.prepare("SELECT repo, path FROM repos").all().map((raw) => `${String(raw["repo"])}\0${String(raw["path"])}`);
    const names = Object.keys(JSON.parse(meta.get("commits") ?? "{}") as Record<string, unknown>);
    if (matchRepos(names, localRepos(root)).some((entry) => !stored.includes(`${entry.repo}\0${entry.path}`))) return null;
    // Whether the file carries contract rows follows the file, not the switch.
    return { digest, writer, contract: meta.get("contract") === "1" };
  } catch {
    return null;
  } finally {
    closeQuietly(db);
    if (otherProject) {
      log(`fact file is for another project than ${project}; replaced`);
      fs.rmSync(file, { force: true });
    }
  }
}

/** Ask the cloud for the project's fact file and bring the local one up to date. Never throws. */
export async function refreshFacts(root: string, options: FactsRefreshOptions = {}): Promise<RefreshOutcome> {
  const env = options.env ?? process.env;
  if (!factsEnabled(env)) return "none";
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
    const known = await knownFile(root, env, project);
    const response = await request(`${API_BASE}/mcp/facts`, {
      method: "POST",
      redirect: "error",
      signal: AbortSignal.timeout(30_000),
      headers: { Authorization: `Bearer ${credential.token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ project, schema: SCHEMA, ...(known ? { known_digest: known.digest, known_writer: known.writer, ...(known.contract ? { known_contract: true } : {}) } : {}) }),
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
    const error = answer["error"] as { code?: unknown; retriable?: unknown } | undefined;
    if (answer["success"] === false || !response.ok) {
      const code = typeof error?.code === "string" ? error.code : `HTTP ${response.status}`;
      const retriable = error?.retriable === true;
      log(`facts not available: ${code}${retriable ? " (retriable)" : ""}`);
      return retriable ? "retry" : "none";
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
    const header = JSON.parse(text.slice(0, text.indexOf("\n") >>> 0)) as { repos?: Array<{ repo?: unknown }> };
    // The header has one entry per service, so a repo of several services is named several times: each name once, in first-seen order.
    const names = [...new Set((header.repos ?? []).map((repo) => repo.repo).filter((repo): repo is string => typeof repo === "string"))];
    const built = await buildFacts(text, factsPath(root), matchRepos(names, localRepos(root)), now, { contract: answer["contract"] === true });
    return built ? "written" : "none";
  } catch (error) {
    log("fact refresh failed", String(error));
    return "none";
  }
}

/**
 * How long `fetchFactsNow` waits on a fact file the cloud is building. The
 * cloud builds a missing file on the first ask, within 25 s
 * (carrick-cloud `docs/internal/reference/edit-facts-wire.md`).
 */
export const FACTS_FIRST_WAIT_MS = 40_000;
/** The pause between two asks while the cloud is building. */
export const FACTS_FIRST_PAUSE_MS = 3_000;

/**
 * Bring the fact file here now, asking again while the cloud is still building
 * it, for at most `waitMs`. `carrick init` runs this so that the first session
 * starts with a fact file: started only by the first session's hooks, the file
 * landed up to a minute into that session, after the agent's first reads and
 * edits. Never throws; a workspace left without a file gets it from the
 * session-start refresh as before.
 */
export async function fetchFactsNow(
  root: string,
  options: FactsRefreshOptions & { waitMs?: number; pauseMs?: number; sleep?: (ms: number) => Promise<void> } = {},
): Promise<RefreshOutcome> {
  const env = options.env ?? process.env;
  if (env["CARRICK_NO_FACTS_REFRESH"]) return "none";
  const pause = options.pauseMs ?? FACTS_FIRST_PAUSE_MS;
  const sleep = options.sleep ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
  for (let asked = 1; ; asked += 1) {
    const outcome = await refreshFacts(root, options);
    if (outcome !== "retry" || asked * pause >= (options.waitMs ?? FACTS_FIRST_WAIT_MS)) return outcome;
    await sleep(pause);
  }
}

/**
 * Start the refresh for `root` detached, at most once per `FACTS_STALE_MS`
 * across every hook and session. Returns whether it started. Never throws.
 */
export function startFactsRefresh(
  root: string,
  options: { env?: NodeJS.ProcessEnv; now?: number; start?: (command: string, args: string[], env: NodeJS.ProcessEnv, cwd?: string) => void } = {},
): boolean {
  const env = options.env ?? process.env;
  if (!factsEnabled(env)) return false;
  if (env["CARRICK_NO_FACTS_REFRESH"]) return false;
  // A folder Carrick was never set up in gets nothing, not even a marker directory.
  if (!hasMarker(root)) return false;
  try {
    const cooldown = cooldownMs(env, "CARRICK_FACTS_COOLDOWN_MS", FACTS_STALE_MS);
    if (!claimRefresh(markerPath(root, FACTS_MARKER), options.now ?? Date.now(), cooldown)) return false;
    const { command, args } = scriptBeside(import.meta.url, "refresh-run", root);
    (options.start ?? startDetached)(command, args, { ...env, CARRICK_NO_KEEP_AWAKE: "1" }, root);
    return true;
  } catch (error) {
    log("fact refresh not started", String(error));
    return false;
  }
}

/**
 * One detached refresh (`refresh-run.ts`): bring the file up to date. After a
 * retriable error the next trigger asks again a minute on; after an answer
 * with no file and no retry it waits `FACTS_FAILED_WAIT_MS`; every other
 * outcome waits out the cooldown.
 */
export async function runFactsRefresh(root: string, options: FactsRefreshOptions = {}): Promise<RefreshOutcome> {
  const outcome = await refreshFacts(root, options);
  log(`fact refresh: ${outcome}`);
  // A new fact file is the hosted index moving: start the move of this
  // machine's copy under its own claim and cooldown (`forwardInBackground`).
  // `status` runs here, in the detached runner, never in a hook's path.
  if (outcome === "written") await startHostedMove(root, options);
  if (outcome === "retry") allowEarlyRetry(root, options.env ?? process.env, options.now ?? Date.now());
  if (outcome === "none") holdAfterFailure(root, options.env ?? process.env, options.now ?? Date.now());
  return outcome;
}

/** The move that follows a written fact file. Never throws. */
async function startHostedMove(root: string, options: FactsRefreshOptions): Promise<void> {
  try {
    const result = options.forward?.status
      ? await options.forward.status()
      : (await readStatus({ cwd: root, workspace: root, env: options.env })).result ?? null;
    if (!result || result.error) return;
    const started = forwardInBackground(root, result, { env: options.env, now: options.now, start: options.forward?.start });
    log(`hosted move after a new fact file: ${started ? "started" : "not started"}`);
  } catch (error) {
    log("hosted move not started", String(error));
  }
}

/**
 * After an answer with no file and no retry, move the claim forward so the
 * next ask waits `FACTS_FAILED_WAIT_MS` from now, not the one-minute cooldown.
 */
export function holdAfterFailure(root: string, env: NodeJS.ProcessEnv = process.env, now: number = Date.now()): void {
  const cooldown = cooldownMs(env, "CARRICK_FACTS_COOLDOWN_MS", FACTS_STALE_MS);
  if (cooldown >= FACTS_FAILED_WAIT_MS) return;
  try {
    fs.writeFileSync(markerPath(root, FACTS_MARKER), `${new Date(now + FACTS_FAILED_WAIT_MS - cooldown).toISOString()}\n`);
  } catch {
    // The next trigger after the ordinary cooldown asks instead.
  }
}

/**
 * After a retriable error, move the claim back so the next trigger, a minute
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
