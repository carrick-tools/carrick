import { setTimeout } from "node:timers/promises";
import { APP_BASE } from "../auth/credentials.ts";
import { openBrowser } from "../auth/oauth.ts";
import { resolveRepos, type ResolvedRepos } from "../auth/read.ts";

type ConnectOptions = {
  interactive: boolean;
  say: (message: string) => void;
  project?: string;
  open?: (url: string) => Promise<boolean>;
  poll?: (signal: AbortSignal) => Promise<ResolvedRepos>;
  wait?: (signal: AbortSignal) => Promise<void>;
  signal?: AbortSignal;
};

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

/** The browser owns installation and assignment; Ctrl-C stops verification. */
export async function connectRepos(token: string, repos: string[], initial: ResolvedRepos, options: ConnectOptions): Promise<ResolvedRepos> {
  const workspaceUrl = `${APP_BASE}/w/${encodeURIComponent(initial.workspace.slug)}`;
  const connectUrl = `${workspaceUrl}/connect`;
  const projectUrl = `${workspaceUrl}/projects`;
  const reposUrl = `${workspaceUrl}/repos`;
  const assignments = new Map<string, string>();

  if (options.project) {
    reportAssignments(initial, repos, assignments, options.say);
    if (reposAreInProject(initial, repos, options.project)) {
      options.say(verifiedLine(repos, options.project));
      return initial;
    }
    options.say(`Create project "${options.project}" if needed: ${projectUrl}`);
    options.say(`  Choose Create project, enter a name, and set the slug to "${options.project}".`);
    if (!initial.workspace.installed || initial.repos.some((repo) => !repo.connected)) {
      options.say(`Connect any missing repos: ${connectUrl}`);
    }
    options.say(`Assign the requested repos: ${reposUrl}`);
    options.say(`  Select ${repos.join(", ")}, choose the target project in "Move selected to", then choose "Move selected".`);
  } else {
    if (initial.workspace.installed && initial.repos.every((repo) => repo.connected)) return initial;
    options.say(`Connect repositories in your browser: ${connectUrl}`);
  }
  if (!options.interactive) {
    if (options.project) {
      options.say(`Project "${options.project}" is not verified for every requested repo.`);
    }
    return initial;
  }
  options.say(
    options.project
      ? `Waiting for every requested repo to reach project "${options.project}". Press Ctrl-C to stop.`
      : "Waiting for repository connections. Press Ctrl-C to continue setup without waiting.",
  );
  const controller = new AbortController();
  const cancel = (): void => controller.abort();
  process.once("SIGINT", cancel);
  const signal = AbortSignal.any([controller.signal, AbortSignal.timeout(30 * 60_000), ...(options.signal ? [options.signal] : [])]);
  let latest = initial;
  const seen = new Set(initial.repos.filter((repo) => repo.connected).map((repo) => repo.full_name));
  try {
    void (options.open ?? openBrowser)(options.project ? projectUrl : connectUrl).catch(() => false);
    while (!signal.aborted) {
      await (options.wait ?? (async (signal) => { await setTimeout(5000, undefined, { signal }); }))(signal);
      signal.throwIfAborted();
      latest = await (options.poll ?? ((signal) => resolveRepos(token, repos, fetch, signal)))(signal);
      if (options.project) {
        reportAssignments(latest, repos, assignments, options.say);
        if (reposAreInProject(latest, repos, options.project)) {
          options.say(verifiedLine(repos, options.project));
          return latest;
        }
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
    options.project
      ? `Stopped waiting; project "${options.project}" is not verified for every requested repo.`
      : "Stopped waiting for repository connections; continuing local setup.",
  );
  return latest;
}
