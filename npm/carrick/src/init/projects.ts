// The workspace's projects, read so every project init prints is printed as
// the dashboard shows it (carrick#955, carrick#1338).
//
// Read only. Projects are created and repos change project on the dashboard
// and through the GitHub App, never from this command (carrick#3361).
//
// **The server may not have the action.** A workspace whose API does not
// answer it gets slugs alone. "Absent" is deliberately wide, because it is not
// one status code: an API that has never heard of an action answers a
// credential-kind gate first, and a later one may answer 404. The only status
// treated as fatal is 401, because the credential was already proven by the
// workspace read that runs before this — a rejection here is a real one and
// must not read as a missing feature.

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

const REJECTED =
  "Carrick rejected this credential. Run carrick login (unset CARRICK_TOKEN first if it overrides your login).";

/**
 * Every project in the signed-in workspace, or null when this API has no such
 * action. Throws only on 401.
 */
export async function listProjects(
  token: string,
  request: typeof fetch = fetch,
  signal?: AbortSignal,
): Promise<Project[] | null> {
  let result: Response;
  try {
    result = await request(`${API_BASE}/types/check-or-upload`, {
      method: "POST",
      redirect: "error",
      signal: AbortSignal.any([AbortSignal.timeout(30_000), ...(signal ? [signal] : [])]),
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ action: "list-projects" }),
    });
  } catch {
    // A request that never reached the server says nothing about whether the
    // action exists, and the workspace read that precedes this one already
    // reports an unreachable API. Fall back rather than fail the run.
    return null;
  }
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

/** The slug shape the dashboard enforces, applied before anything is sent. */
export const SLUG = /^[a-z0-9](?:[a-z0-9]|-(?=[a-z0-9])){2,31}$/;

/**
 * A project as it is printed: the name the dashboard shows it under, and the
 * slug every command takes.
 *
 * The CLI named projects by slug and the dashboard's picker names them by
 * display name, with nothing connecting the two (carrick#1338). Both, wherever
 * a project is printed. A workspace whose API does not serve the project list
 * has only the slug, and that is what it gets.
 */
export function projectLabel(slug: string, projects: Project[] | null): string {
  const found = projects?.find((project) => project.slug === slug);
  return found && found.name !== slug ? `${found.name} (${slug})` : slug;
}
