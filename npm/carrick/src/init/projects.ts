// Listing, creating and filling Carrick projects from the terminal
// (carrick#955, carrick#999).
//
// `carrick init --project <slug>` could only ever point at a browser: the
// workspace read (`resolve-repos`) returns the projects the named repos are
// already in, never the workspace's project list, and creation and assignment
// lived only in the dashboard. These three actions are the terminal half, and
// they leave the GitHub App grant as the only browser step.
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

/** What one repo's assignment did, as the server reports it (carrick#999). */
const placement = z.object({
  full_name: z.string(),
  /** This repo is in the project now, including one that already was. */
  assigned: z.boolean(),
  /** Whether a write happened. False for a repo that was already there. */
  moved: z.boolean(),
  project_slug: z.string().nullable(),
  /** Why this repo was not placed, as a sentence completing its name. */
  reason: z.string().nullable(),
});

/** One repo's outcome inside a successful `assign-repos` answer. */
export type Placement = z.infer<typeof placement>;

const assignResponse = z.object({
  schema: z.literal("carrick.assign-repos/0"),
  project_slug: z.string(),
  repos: z.array(placement),
});

/** What a create attempt did. `absent` is "this server has no such action". */
export type CreateOutcome =
  | { kind: "created"; project: Project }
  | { kind: "refused"; message: string }
  | { kind: "absent" };

/**
 * What an assignment attempt did.
 *
 * `placed` is a 200: the action ran, and every repo in it carries its own
 * outcome. A repo the workspace does not hold, or one whose write failed, is
 * reported there rather than as a refusal, so a partial result is read rather
 * than discarded.
 */
export type AssignOutcome =
  | { kind: "placed"; repos: Placement[] }
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

/**
 * Put the named repos in the named project, where this API can do it.
 *
 * The second browser step this command used to end on: the GitHub App grant
 * puts a newly connected repo in the workspace's default project, so a run
 * started with `--project` sat waiting for someone to move it on the Repos
 * page (carrick#999). The action is idempotent, which is what makes it safe
 * to call from inside the polling loop as repos arrive from the grant.
 *
 * `absent` is the same wide condition the other two actions use: this client
 * is published ahead of the deploy that serves the action, and until it lands
 * the credential-kind gate answers first. The caller then prints the browser
 * instruction it printed before.
 */
export async function assignRepos(
  token: string,
  project: string,
  repos: string[],
  request: typeof fetch = fetch,
  signal?: AbortSignal,
): Promise<AssignOutcome> {
  const result = await post(token, { action: "assign-repos", project, repos }, request, signal);
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
  const parsed = assignResponse.safeParse(payload);
  // The schema tag is the handshake, as it is for the list: an answer this
  // client cannot read is the same situation as an API without the action.
  return parsed.success && parsed.data.project_slug === project
    ? { kind: "placed", repos: parsed.data.repos }
    : { kind: "absent" };
}

/** The slug shape the dashboard enforces, applied before anything is sent. */
export const SLUG = /^[a-z0-9](?:[a-z0-9]|-(?=[a-z0-9])){2,31}$/;

/** What a project is, said once, above the picker that asks for one. */
export const PROJECT_RULE =
  "One project per interconnected system: repos that call each other belong together.";

/** What init asks the terminal during the project step. */
export type ProjectPrompts = {
  say: (line: string) => void;
  /** A free-text answer, already trimmed. Empty means "skip this". */
  ask: (question: string) => Promise<string>;
  confirm: (question: string) => Promise<boolean>;
  interactive: boolean;
  assumeYes: boolean;
  list?: (token: string) => Promise<Project[] | null>;
  create?: (token: string, slug: string) => Promise<CreateOutcome>;
};

/**
 * The project these repos should be in, and whether this run can state that it
 * exists.
 *
 * `slug: null` is the honest answer whenever the terminal could not settle it:
 * an API without the project actions, a workspace whose repos are spread
 * across projects with nobody at the keyboard, a declined offer. The caller
 * then does what it did before this step existed — connect the repos and leave
 * the project to the browser.
 */
export type ProjectChoice = { slug: string | null; exists: boolean };

/**
 * Make sure a named project exists, from the terminal where that is possible.
 *
 * Returns whether this run can state that it exists. `false` covers every case
 * it could not settle — an API without the actions, a declined offer, a
 * refused create — and the caller then prints the browser instructions, which
 * include creating it.
 */
export async function ensureProject(
  token: string,
  slug: string,
  prompts: ProjectPrompts,
): Promise<boolean> {
  const { say } = prompts;
  const projects = await (prompts.list ?? ((token: string) => listProjects(token)))(token);
  if (projects === null) return false;
  if (projects.some((project) => project.slug === slug && !project.archived)) {
    say(`Project "${slug}" is in this workspace.`);
    return true;
  }
  say(projects.length === 0 ? "This workspace has no projects yet." : "Projects in this workspace:");
  for (const line of projectLines(projects)) say(line);
  const create =
    prompts.assumeYes || (prompts.interactive && (await prompts.confirm(`Create project "${slug}"?`)));
  if (!create) return false;
  const outcome = await (prompts.create ?? ((token, slug) => createProject(token, slug)))(token, slug);
  if (outcome.kind === "created") {
    say(`Created project "${slug}".`);
    return true;
  }
  if (outcome.kind === "refused") say(`Carrick did not create "${slug}": ${outcome.message}`);
  return false;
}

/**
 * The project step for a plain `carrick init`, with no `--project` to verify
 * (carrick#987, decision record carrick-cloud#772 step 3).
 *
 * Without the flag this used to do nothing at all, so a first run never saw
 * the workspace's projects and the repo silently stayed wherever the browser
 * had put it. The order is: the assignment the repos already have, then the
 * list, then a name the user types, which may be one that does not exist yet.
 * Assignment itself is still the browser's, so everything settled here only
 * removes a step from the wait.
 *
 * `current` is one entry per requested repo: the project it is in, or null
 * when it is not connected to this workspace at all.
 */
export async function projectStep(
  token: string,
  current: Array<string | null>,
  prompts: ProjectPrompts,
): Promise<ProjectChoice> {
  const list = prompts.list ?? ((token: string) => listProjects(token));
  if (current.length === 0) return { slug: null, exists: false };

  const assigned = [...new Set(current)];
  const sole = assigned.length === 1 ? assigned[0] : null;
  if (sole != null) {
    // Every repo is connected and in one project. That is the answer unless
    // someone at the keyboard says otherwise, and the caller states the
    // assignment it verifies, so this says nothing of its own.
    if (!prompts.interactive || prompts.assumeYes) return { slug: sole, exists: true };
    if (await prompts.confirm(`These repos are in project "${sole}". Keep them there?`)) {
      return { slug: sole, exists: true };
    }
  }
  // Nothing to ask without a terminal, and nothing this run may assume: a
  // repo's project is not a thing to guess at.
  if (!prompts.interactive) return { slug: null, exists: false };

  const projects = await list(token);
  if (projects === null) return { slug: null, exists: false };
  // The one thing someone picking a project has to know, said where the
  // question is asked: a project is the boundary every cross-service answer
  // is computed inside, so repos split across two of them see nothing of each
  // other (carrick#993).
  prompts.say(PROJECT_RULE);
  prompts.say(
    projects.length === 0 ? "This workspace has no projects yet." : "Projects in this workspace:",
  );
  for (const line of projectLines(projects)) prompts.say(line);
  const answer = await prompts.ask(
    "Which project should these repos be in? Enter a slug, or press Enter to leave it to the browser.",
  );
  if (answer === "") return { slug: null, exists: false };
  if (!SLUG.test(answer)) {
    prompts.say(
      `"${answer}" is not a project slug: use 3-32 lowercase letters, digits, and single hyphens. Leaving the project step to the browser.`,
    );
    return { slug: null, exists: false };
  }
  if (projects.some((project) => project.slug === answer && !project.archived)) {
    return { slug: answer, exists: true };
  }
  const create = await prompts.confirm(`Create project "${answer}"?`);
  if (!create) return { slug: answer, exists: false };
  const outcome = await (prompts.create ?? ((token, slug) => createProject(token, slug)))(token, answer);
  if (outcome.kind === "created") {
    prompts.say(`Created project "${answer}".`);
    return { slug: answer, exists: true };
  }
  if (outcome.kind === "refused") prompts.say(`Carrick did not create "${answer}": ${outcome.message}`);
  return { slug: answer, exists: false };
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
