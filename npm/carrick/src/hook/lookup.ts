// The prompt lookup: on a task-sized prompt, name up to three functions the
// project already has that may do part of it (carrick#3375).
//
// Why at the prompt. The agent decides to write a helper in its first steps,
// before any edit exists for an edit-time check to read, and a measured run
// found the existing helper, read it, and wrote its own copy anyway. The
// prompt is the earliest text that says what the task is, and the index ranks
// functions by what they do, so one `search-by-intent` call on it names the
// helper before the first edit.
//
// What leaves the machine. The first 1,000 characters of the prompt, once, in
// the HTTPS body of one `search-by-intent` call (the server's own query cap,
// the same lambda path the MCP tool uses), and the lambda logs counts only.
// This module writes the prompt nowhere: not the session record, not a log, not
// stdout. Its one log line is counts. A failure of any kind, a refusal, or the
// deadline prints nothing: a prompt is never held up past `DEADLINE_MS`.
//
// Reference: `docs/reference/task-skills.md`, "The prompt lookup".

import fs from "node:fs";
import path from "node:path";
import { post } from "../auth/api.ts";
import { readCredential } from "../auth/credentials.ts";
import { invokerValue } from "./invoker.ts";
import { countLookup, markNamedByLookup, readSession } from "./reuse.ts";
import type { Logger } from "../log.ts";

/** A prompt shorter than this is a question, not a task. */
export const MIN_WORDS = 12;
/** The most lookups one session makes. */
export const MAX_LOOKUPS = 10;
/** The server's own cap on a query (`MAX_QUERY_CHARS`). */
export const QUERY_CHARS = 1000;
/** The most functions named. */
export const TOP_K = 3;
/** The hard time budget for the whole lookup. */
export const DEADLINE_MS = 3000;

/**
 * The one project id the workspace's cached hosted snapshot names, or null
 * when there is none or more than one (a lookup is one project's question).
 * `snapshot.json` is written by the hosted refresh: `resolution.repos[]`
 * carries each connected repo's `project_id`.
 */
export function snapshotProjectId(root: string): { id: string | null; distinct: number } {
  let parsed: unknown;
  try {
    parsed = JSON.parse(fs.readFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), "utf8"));
  } catch {
    return { id: null, distinct: 0 };
  }
  const repos = (parsed as { resolution?: { repos?: unknown } } | null)?.resolution?.repos;
  if (!Array.isArray(repos)) return { id: null, distinct: 0 };
  const ids = new Set<string>();
  for (const repo of repos) {
    const id = (repo as { project_id?: unknown } | null)?.project_id;
    if (typeof id === "string" && id !== "") ids.add(id);
  }
  return { id: ids.size === 1 ? [...ids][0]! : null, distinct: ids.size };
}

/** Words in a prompt, the way a person counts them. */
export function wordCount(prompt: string): number {
  return prompt.split(/\s+/).filter((word) => word !== "").length;
}

type Row = { name?: unknown; file_path?: unknown; line_number?: unknown };

/** The named rows of an answer, as the server ranked them, at most `TOP_K`. */
export function namedRows(payload: unknown): { name: string; where: string }[] {
  const results = (payload as { results?: unknown } | null)?.results;
  if (!Array.isArray(results)) return [];
  const rows: { name: string; where: string }[] = [];
  for (const row of results as Row[]) {
    if (typeof row?.name !== "string" || row.name === "") continue;
    if (typeof row.file_path !== "string" || row.file_path === "") continue;
    const line = typeof row.line_number === "number" ? `:${row.line_number}` : "";
    rows.push({ name: row.name, where: `${row.file_path}${line}` });
    if (rows.length === TOP_K) break;
  }
  return rows;
}

/** The line the model is handed. */
export function lookupLine(rows: { name: string; where: string }[]): string {
  const list = rows.map((row) => `\`${row.name}\` (\`${row.where}\`)`).join(", ");
  return `Carrick: functions that may already do part of this: ${list}. If one does the job, import it and call it rather than writing your own.`;
}

export type LookupOptions = {
  prompt: string;
  session: string;
  /** The folder `carrick init` ran in. */
  root: string;
  env?: NodeJS.ProcessEnv;
  home?: string;
  request?: typeof fetch;
  log: Logger;
  deadlineMs?: number;
};

/**
 * The line for this prompt, or null. Never throws; the prompt text is only
 * ever read here, into the one request body.
 */
export async function promptLookup(options: LookupOptions): Promise<string | null> {
  const env = options.env ?? process.env;
  const { log } = options;
  const words = wordCount(options.prompt);
  if (words < MIN_WORDS) return null;

  let token: string;
  try {
    const credential = readCredential(env);
    if (credential === null) {
      log("lookup skipped: no credential");
      return null;
    }
    token = credential.token;
  } catch {
    log("lookup skipped: credential unreadable");
    return null;
  }

  const project = snapshotProjectId(options.root);
  if (project.id === null) {
    log(`lookup skipped: ${project.distinct} project id(s) in the snapshot`);
    return null;
  }

  // Counted before the request, so a refused or timed-out call still spends one.
  if (readSession(options.session, options.home).lookups >= MAX_LOOKUPS) {
    log(`lookup skipped: ${MAX_LOOKUPS} lookups already made this session`);
    return null;
  }
  countLookup(options.session, options.home);

  const started = Date.now();
  const deadline = options.deadlineMs ?? DEADLINE_MS;
  const controller = AbortSignal.timeout(deadline);
  // A fetch that ignores its signal must still not hold the prompt.
  const cut = new Promise<null>((resolve) => {
    setTimeout(() => resolve(null), deadline).unref();
  });
  const sent = post(
    token,
    {
      action: "search-by-intent",
      project_id: project.id,
      query: options.prompt.slice(0, QUERY_CHARS),
      top_k: TOP_K,
    },
    options.request ?? fetch,
    controller,
    { "x-carrick-invoker": invokerValue("user-prompt") },
  );
  const result = await Promise.race([sent, cut]);
  const ms = Date.now() - started;
  if (result === null) {
    log(`lookup words=${words} ms=${ms} rows=0 status=none`);
    return null;
  }
  let rows: { name: string; where: string }[] = [];
  if (result.ok) {
    try {
      rows = namedRows(await Promise.race([result.json(), cut.then(() => null)]));
    } catch {
      rows = [];
    }
  }
  log(`lookup words=${words} ms=${Date.now() - started} rows=${rows.length} status=${result.status}`);
  if (rows.length === 0) return null;
  markNamedByLookup(
    options.session,
    rows.map((row) => `${row.where.replace(/:\d+$/, "")}::${row.name}`),
    options.home,
  );
  return lookupLine(rows);
}
