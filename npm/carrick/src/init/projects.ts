// Listing and creating Carrick projects from the terminal (carrick#955).
//
// `carrick init --project <slug>` could only ever point at a browser: the
// workspace read (`resolve-repos`) returns the projects the named repos are
// already in, never the workspace's project list, and creation lived only in
// the dashboard. These two actions are the terminal half.
//
// **The server may not have them.** They are a separate cloud change, so
// every call here is a probe: a workspace whose API does not answer them gets
// today's browser links and the same wait, and the release does not depend on
// a deploy. "Absent" is deliberately wide, because it is not one status code:
// an API that has never heard of an action answers a credential-kind gate
// first (403 today, since the CLI credential is an MCP-scoped key and the
// action is not on that key's read list), and a later one may answer 404. The
// only status treated as fatal is 401, because the credential was already
// proven by the workspace read that runs before this — a rejection here is a
// real one and must not read as a missing feature.
//
// A refusal is separated from an absence by the pair (status, body): a 400 or
// a 409 carrying an `error` string is the server stating why a create cannot
// happen (slug taken, reserved, rate limited), and that sentence is the
// user's to read. Everything else is an absence.

import { z } from "zod";
import { API_BASE } from "../auth/credentials.ts";

const project = z.object({
  slug: z.string(),
  name: z.string(),
  archived: z.boolean(),
  repo_count: z.number().int().nonnegative().nullable(),
});

/** One project in the signed-in workspace. */
export type Project = z.infer<typeof project>;

const listResponse = z.object({
  schema: z.literal("carrick.list-projects/0"),
  projects: z.array(project),
});

const createResponse = z.object({
  schema: z.literal("carrick.create-project/0"),
  project,
});

/** What a create attempt did. `absent` is "this server has no such action". */
export type CreateOutcome =
  | { kind: "created"; project: Project }
  | { kind: "refused"; message: string }
  | { kind: "absent" };

const REJECTED =
  "Carrick rejected this credential. Run carrick login (unset CARRICK_TOKEN first if it overrides your login).";

async function post(
  token: string,
  body: Record<string, unknown>,
  request: typeof fetch,
  signal?: AbortSignal,
): Promise<Response | null> {
  try {
    return await request(`${API_BASE}/types/check-or-upload`, {
      method: "POST",
      redirect: "error",
      signal: AbortSignal.any([AbortSignal.timeout(30_000), ...(signal ? [signal] : [])]),
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
  } catch {
    // A request that never reached the server says nothing about whether the
    // action exists, and the workspace read that precedes this one already
    // reports an unreachable API. Fall back rather than fail the run.
    return null;
  }
}

/** The `error` sentence a refusal carries, when it carries one. */
async function refusal(result: Response): Promise<string | null> {
  if (result.status !== 400 && result.status !== 409) return null;
  try {
    const payload: unknown = await result.json();
    const message = (payload as { error?: unknown } | null)?.error;
    return typeof message === "string" && message.trim() !== "" ? message : null;
  } catch {
    return null;
  }
}

/**
 * Every project in the signed-in workspace, or null when this API has no such
 * action. Throws only on 401.
 */
export async function listProjects(
  token: string,
  request: typeof fetch = fetch,
  signal?: AbortSignal,
): Promise<Project[] | null> {
  const result = await post(token, { action: "list-projects" }, request, signal);
  if (!result) return null;
  if (result.status === 401) throw new Error(REJECTED);
  if (!result.ok) return null;
  let payload: unknown;
  try {
    payload = await result.json();
  } catch {
    return null;
  }
  const parsed = listResponse.safeParse(payload);
  // The schema tag is the handshake. An API that answers this action with
  // some other shape is one this client cannot read, which is the same
  // situation as not having it.
  return parsed.success ? parsed.data.projects : null;
}

/**
 * Create one project in the signed-in workspace.
 *
 * The slug is sent as given: `carrick init` validates it against the same
 * shape the dashboard enforces before it ever reaches here, so a slug this
 * client sends is one the server can accept or refuse on its own rules, never
 * one it has to normalise silently into something the user did not ask for.
 */
export async function createProject(
  token: string,
  slug: string,
  name: string = slug,
  request: typeof fetch = fetch,
  signal?: AbortSignal,
): Promise<CreateOutcome> {
  const result = await post(token, { action: "create-project", slug, name }, request, signal);
  if (!result) return { kind: "absent" };
  if (result.status === 401) throw new Error(REJECTED);
  if (!result.ok) {
    const message = await refusal(result);
    return message ? { kind: "refused", message } : { kind: "absent" };
  }
  let payload: unknown;
  try {
    payload = await result.json();
  } catch {
    return { kind: "absent" };
  }
  const parsed = createResponse.safeParse(payload);
  return parsed.success ? { kind: "created", project: parsed.data.project } : { kind: "absent" };
}

/** The list as init prints it: one line per project, active ones first. */
export function projectLines(projects: Project[]): string[] {
  const ordered = [...projects].sort((left, right) =>
    left.archived === right.archived
      ? left.slug.localeCompare(right.slug)
      : Number(left.archived) - Number(right.archived),
  );
  return ordered.map((entry) => {
    const repos =
      entry.repo_count === null
        ? ""
        : `  ${entry.repo_count} repo${entry.repo_count === 1 ? "" : "s"}`;
    const archived = entry.archived ? "  (archived)" : "";
    const named = entry.name === entry.slug ? "" : `  ${entry.name}`;
    return `  ${entry.slug}${named}${repos}${archived}`;
  });
}
