import { setTimeout } from "node:timers/promises";
import { APP_BASE } from "../auth/credentials.ts";
import { openBrowser } from "../auth/oauth.ts";
import { resolveRepos, type ResolvedRepos } from "../auth/read.ts";
import { assignRepos, type AssignOutcome } from "./projects.ts";

type ConnectOptions = {
  interactive: boolean;
  say: (message: string) => void;
  project?: string;
  /** True when this run has already seen or created the project (carrick#955). */
  projectExists?: boolean;
  open?: (url: string) => Promise<boolean>;
  poll?: (signal?: AbortSignal) => Promise<ResolvedRepos>;
  /** Placing repos in the project, injected so tests state the server (carrick#999). */
  assign?: (repos: string[], signal?: AbortSignal) => Promise<AssignOutcome>;
  wait?: (signal: AbortSignal) => Promise<void>;
  signal?: AbortSignal;
};

/**
 * The line before every wait.
 *
 * What is waited on is a page in the Carrick dashboard, and both of them —
 * the App grant and the Repos page — refuse anyone who is not an owner or an
 * admin of the workspace. Waiting thirty minutes on a page you may not use is
 * the friction this says out loud (carrick#993).
 */
export const ADMIN_WAIT =
  "A workspace owner or admin must do this; Ctrl-C and run init again once they have.";

/** Every requested repo must be present, connected, and assigned to this project. */
export function reposAreInProject(
  identity: ResolvedRepos,
  repos: string[],
  project: string,
): boolean {
  if (!identity.workspace.installed || repos.length === 0) return false;
  return repos.every((name) => {
    const repo = identity.repos.find(
      (candidate) => candidate.full_name.toLowerCase() === name.toLowerCase(),
    );
    return repo?.connected === true && repo.project_slug === project;
  });
}

function assignment(identity: ResolvedRepos, name: string): string {
  const repo = identity.repos.find(
    (candidate) => candidate.full_name.toLowerCase() === name.toLowerCase(),
  );
  return repo?.connected === true ? repo.project_slug : "not connected";
}

/**
 * The project each requested repo is in, in the order they were requested,
 * with null for one this workspace does not hold.
 *
 * The project step reads this before it asks anything: a repo that is already
 * in a project answers the question on its own (carrick#987).
 */
export function projectAssignments(
  identity: ResolvedRepos,
  repos: string[],
): Array<string | null> {
  return repos.map((name) => {
    const repo = identity.repos.find(
      (candidate) => candidate.full_name.toLowerCase() === name.toLowerCase(),
    );
    return repo?.connected === true ? repo.project_slug : null;
  });
}

function reportAssignments(
  identity: ResolvedRepos,
  repos: string[],
  previous: Map<string, string>,
  say: (message: string) => void,
): void {
  for (const name of repos) {
    const current = assignment(identity, name);
    if (previous.get(name.toLowerCase()) === current) continue;
    const repo = identity.repos.find(
      (candidate) => candidate.full_name.toLowerCase() === name.toLowerCase(),
    );
    const actualName = repo?.full_name ?? name;
    say(
      current === "not connected"
        ? `${actualName} is not connected to this workspace.`
        : `${actualName} is currently in project "${current}".`,
    );
    previous.set(name.toLowerCase(), current);
  }
}

function verifiedLine(repos: string[], project: string): string {
  return repos.length === 1
    ? `Verified 1 repo in project "${project}".`
    : `Verified ${repos.length} repos in project "${project}".`;
}

/**
 * The repos this run may still place: connected to the workspace, and in some
 * other project than the one asked for.
 */
function misplaced(identity: ResolvedRepos, repos: string[], project: string): string[] {
  return repos.filter((name) => {
    const repo = identity.repos.find(
      (candidate) => candidate.full_name.toLowerCase() === name.toLowerCase(),
    );
    return repo?.connected === true && repo.project_slug !== project;
  });
}

/**
 * The GitHub App grant is the only browser step; the CLI places the repos.
 *
 * Assignment is a server action on this credential (carrick#999), tried for
 * every connected repo the moment the poll sees it, so nobody has to open the
 * Repos page and move it by hand. An API that does not serve the action yet —
 * which is every API until the deploy lands — answers an absence, and the
 * browser instruction this command printed before is what it falls back to.
 * Ctrl-C stops verification.
 */
export async function connectRepos(token: string, repos: string[], initial: ResolvedRepos, options: ConnectOptions): Promise<ResolvedRepos> {
  const workspaceUrl = `${APP_BASE}/w/${encodeURIComponent(initial.workspace.slug)}`;
  const connectUrl = `${workspaceUrl}/connect`;
  const projectUrl = `${workspaceUrl}/projects`;
  const reposUrl = `${workspaceUrl}/repos`;
  const assignments = new Map<string, string>();
  const poll = options.poll ?? ((signal?: AbortSignal) => resolveRepos(token, repos, fetch, signal));
  const project = options.project;
  const assign =
    options.assign ??
    ((names: string[], signal?: AbortSignal) => assignRepos(token, project ?? "", names, fetch, signal));
  // A project this run has not seen in the workspace may not exist at all, so
  // there is nothing to place repos into and the browser owns both steps.
  let placeable = project !== undefined && options.projectExists === true;
  let saidBrowserAssign = false;
  // One line per distinct thing the server said, so a write that keeps failing
  // does not print the same sentence every five seconds.
  const said = new Set<string>();

  const sayOnce = (line: string): void => {
    if (said.has(line)) return;
    said.add(line);
    options.say(line);
  };

  const browserAssign = (): void => {
    if (saidBrowserAssign) return;
    saidBrowserAssign = true;
    options.say(`Assign the requested repos: ${reposUrl}`);
    options.say(`  Select ${repos.join(", ")}, choose the target project in "Move selected to", then choose "Move selected".`);
  };

  /** Place what can be placed; true when something moved and is worth re-reading. */
  const place = async (identity: ResolvedRepos, signal?: AbortSignal): Promise<boolean> => {
    if (!placeable || project === undefined) return false;
    const pending = misplaced(identity, repos, project);
    if (pending.length === 0) return false;
    const outcome = await assign(pending, signal);
    if (outcome.kind !== "placed") {
      // Neither a refusal nor an absence is worth retrying every five seconds,
      // and both leave the assignment to the browser. The instruction itself
      // is printed by the caller, after the steps that come before it.
      placeable = false;
      if (outcome.kind === "refused") {
        options.say(`Carrick did not assign the requested repos to "${project}": ${outcome.message}`);
      }
      return false;
    }
    let moved = false;
    for (const repo of outcome.repos) {
      if (repo.assigned) {
        moved = moved || repo.moved;
        if (repo.moved) sayOnce(`Moved ${repo.full_name} into project "${project}".`);
      } else {
        sayOnce(`${repo.full_name} was not moved: ${repo.reason ?? "Carrick gave no reason."}`);
      }
    }
    return moved;
  };

  /** Place, then read the assignment back: a claim is only made on the read. */
  const settle = async (identity: ResolvedRepos, signal?: AbortSignal): Promise<ResolvedRepos> => {
    if (!(await place(identity, signal))) return identity;
    const latest = await poll(signal);
    reportAssignments(latest, repos, assignments, options.say);
    return latest;
  };

  let latest = initial;
  if (project !== undefined) {
    reportAssignments(latest, repos, assignments, options.say);
    if (reposAreInProject(latest, repos, project)) {
      options.say(verifiedLine(repos, project));
      return latest;
    }
    latest = await settle(latest, options.signal);
    if (reposAreInProject(latest, repos, project)) {
      options.say(verifiedLine(repos, project));
      return latest;
    }
    // The create step is skipped only when this run has just seen the project
    // in the workspace or created it.
    if (!options.projectExists) {
      options.say(`Create project "${project}" if needed: ${projectUrl}`);
      options.say(`  Choose Create project, enter a name, and set the slug to "${project}".`);
    }
    if (!latest.workspace.installed || latest.repos.some((repo) => !repo.connected)) {
      options.say(`Connect any missing repos: ${connectUrl}`);
    }
    // Only where this run cannot do the placing itself. With repos still to
    // connect it does not know yet, and says so when the poll finds out.
    if (!placeable) browserAssign();
  } else {
    if (latest.workspace.installed && latest.repos.every((repo) => repo.connected)) return latest;
    options.say(`Connect repositories in your browser: ${connectUrl}`);
  }
  if (!options.interactive) {
    if (project !== undefined) {
      options.say(`Project "${project}" is not verified for every requested repo.`);
    }
    return latest;
  }
  options.say(ADMIN_WAIT);
  options.say(
    project !== undefined
      ? `Waiting for every requested repo to reach project "${project}". Press Ctrl-C to stop.`
      : "Waiting for repository connections. Press Ctrl-C to continue setup without waiting.",
  );
  const controller = new AbortController();
  const cancel = (): void => controller.abort();
  process.once("SIGINT", cancel);
  const signal = AbortSignal.any([controller.signal, AbortSignal.timeout(30 * 60_000), ...(options.signal ? [options.signal] : [])]);
  const seen = new Set(latest.repos.filter((repo) => repo.connected).map((repo) => repo.full_name));
  try {
    // Where the browser is pointed: at the project it must create, else at the
    // grant that connects the repos, else — only where this run cannot place
    // them itself — at the page that moves them.
    const target =
      project === undefined
        ? connectUrl
        : !options.projectExists
          ? projectUrl
          : !latest.workspace.installed || latest.repos.some((repo) => !repo.connected)
            ? connectUrl
            : reposUrl;
    void (options.open ?? openBrowser)(target).catch(() => false);
    while (!signal.aborted) {
      await (options.wait ?? (async (signal) => { await setTimeout(5000, undefined, { signal }); }))(signal);
      signal.throwIfAborted();
      latest = await poll(signal);
      if (project !== undefined) {
        reportAssignments(latest, repos, assignments, options.say);
        latest = await settle(latest, signal);
        if (reposAreInProject(latest, repos, project)) {
          options.say(verifiedLine(repos, project));
          return latest;
        }
        if (!placeable) browserAssign();
      } else {
        for (const repo of latest.repos) {
          if (repo.connected && !seen.has(repo.full_name)) {
            options.say(`Connected ${repo.full_name}.`);
            seen.add(repo.full_name);
          }
        }
        if (latest.workspace.installed && latest.repos.every((repo) => repo.connected)) return latest;
      }
    }
  } catch (error) { if (!signal.aborted) throw error; }
  finally { process.removeListener("SIGINT", cancel); }
  options.say(
    project !== undefined
      ? `Stopped waiting; project "${project}" is not verified for every requested repo.`
      : "Stopped waiting for repository connections; continuing local setup.",
  );
  return latest;
}
