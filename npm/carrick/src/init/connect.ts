import { setTimeout } from "node:timers/promises";
import { APP_BASE } from "../auth/credentials.ts";
import { openBrowser } from "../auth/oauth.ts";
import { resolveRepos, type ResolvedRepos } from "../auth/read.ts";
import { assignRepos, type AssignOutcome } from "./projects.ts";
import type { StepProgress, StepReport } from "./output.ts";

/**
 * Show a status that updates in place while `work` runs, then its report.
 *
 * `InitOutput.step` in a run: a spinner on a terminal, one line per change on
 * a pipe. The default is the pipe's behaviour, for a caller with no output.
 */
export type Track = <T>(
  label: string,
  work: (progress: StepProgress) => Promise<T>,
  report: (value: T) => StepReport,
) => Promise<T>;

type ConnectOptions = {
  interactive: boolean;
  say: (message: string) => void;
  project?: string;
  /** True when this run has already seen or created the project (carrick#955). */
  projectExists?: boolean;
  /**
   * The repos this run may take OUT of another project, lowercased.
   *
   * A move changes what every agent querying either project can see, so it is
   * named in the proposal and consented to there; this set is that consent
   * arriving (carrick#1338). A repo that was not connected at all when the
   * proposal was read is in it too: the App grant lands it in the workspace's
   * default project seconds later, and placing it where the run asked for is
   * the grant finishing rather than a move out of a project anyone chose.
   */
  movable?: Set<string>;
  /**
   * The movable repos whose move is said, lowercased: the ones in a project
   * somebody chose. A repo the App grant has just put in the default project
   * moves silently, because nobody chose where it was (carrick#1489).
   */
  announce?: Set<string>;
  /** How a project is printed: display name beside slug, where one is known. */
  label?: (slug: string) => string;
  /**
   * True where the workspace read says the signed-in user is a member, not an
   * owner or an admin. Only they are told who can finish a browser step: an
   * owner told to go and find an owner was the line in carrick#1512.
   */
  member?: boolean;
  /** How to run init again, for the member's line: "run carrick init in ~/shop again". */
  again?: string;
  open?: (url: string) => Promise<boolean>;
  poll?: (signal?: AbortSignal) => Promise<ResolvedRepos>;
  /** Placing repos in the project, injected so tests state the server (carrick#999). */
  assign?: (repos: string[], signal?: AbortSignal) => Promise<AssignOutcome>;
  wait?: (signal: AbortSignal) => Promise<void>;
  /** The live status line the wait is shown on (carrick#1489). */
  track?: Track;
  signal?: AbortSignal;
};

/**
 * The line before a member's wait.
 *
 * What is waited on is a page in the Carrick dashboard, and both of them —
 * the App grant and the Repos page — refuse anyone who is not an owner or an
 * admin of the workspace. Waiting thirty minutes on a page you may not use is
 * the friction this says out loud (carrick#993), to the members it is true of
 * (carrick#1512). `again` is how to run init again, which names the folder
 * when the run set up the one above where it started.
 */
export function adminWait(again: string = "run carrick init again"): string {
  return `A workspace owner or admin must do this; Ctrl-C and ${again} once they have.`;
}

/** The dashboard pages init sends a reader to, for one workspace. */
export function workspaceUrls(slug: string): { connect: string; projects: string; repos: string } {
  const workspace = `${APP_BASE}/w/${encodeURIComponent(slug)}`;
  return { connect: `${workspace}/connect`, projects: `${workspace}/projects`, repos: `${workspace}/repos` };
}

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

/** The requested repos this read says are not connected, as the server spells them. */
export function unconnectedRepos(identity: ResolvedRepos, repos: string[]): string[] {
  return repos
    .map((name) => ({
      name,
      row: identity.repos.find((candidate) => candidate.full_name.toLowerCase() === name.toLowerCase()),
    }))
    .filter(({ row }) => !identity.workspace.installed || row?.connected !== true)
    .map(({ name, row }) => row?.full_name ?? name);
}

/**
 * The line that sends a reader to the GitHub App, naming the repos it is for,
 * for a run that cannot open the page itself: one with no terminal, and one
 * whose wait was stopped with the grant still to do. Printed only for repos a
 * read taken just now says are unconnected: the grant page re-asks GitHub for
 * the whole repo list, which is wasted on a repo that is already connected
 * (carrick#1489).
 */
export function connectLine(unconnected: string[], total: number, url: string): string {
  const which =
    unconnected.length === 1
      ? `${unconnected[0]} is not connected yet`
      : unconnected.length === total && total === 2
        ? "Neither repo is connected yet"
        : unconnected.length === total
          ? `None of the ${total} repos is connected yet`
          : `${unconnected.length} of ${total} repos are not connected yet`;
  const on = unconnected.length === 1 ? "it" : total === 2 && unconnected.length === 2 ? "both" : "them";
  return `${which}. Install the Carrick GitHub App on ${on}: ${url}`;
}

/** "1 repo", "2 repos". */
function repoCount(count: number): string {
  return `${count} repo${count === 1 ? "" : "s"}`;
}

/**
 * The repos this run may still place: connected to the workspace, in some
 * other project than the one asked for, and consented to in the proposal.
 *
 * A connected repo that was not consented to is left exactly where it is. The
 * caller says so once and prints the browser step, because a project move is
 * not something a `--yes` to a list of packages grants (carrick#1338).
 */
function misplaced(
  identity: ResolvedRepos,
  repos: string[],
  project: string,
  movable: Set<string>,
): string[] {
  return repos.filter((name) => {
    const repo = identity.repos.find(
      (candidate) => candidate.full_name.toLowerCase() === name.toLowerCase(),
    );
    return (
      repo?.connected === true && repo.project_slug !== project && movable.has(name.toLowerCase())
    );
  });
}

/** The pipe's rendering of a tracked wait: each distinct status once, then the report. */
function plainTrack(say: (line: string) => void): Track {
  return async (_label, work, report) => {
    const value = await work((text) => say(text));
    say(report(value).text);
    return value;
  };
}

/**
 * The GitHub App grant is the only browser step; the CLI places the repos.
 *
 * Assignment is a server action on this credential (carrick#999), tried for
 * every connected repo the moment the poll sees it, so nobody has to open the
 * Repos page and move it by hand. An API that does not serve the action yet
 * answers an absence, and the browser instruction is what it falls back to.
 * Ctrl-C stops verification.
 *
 * What it prints is the state and the next step, not the traffic
 * (carrick#1489): the wait is one status line that updates in place, a move
 * the run was allowed to make is not announced (the first one out of an
 * untouched default project is the App grant finishing, not a decision), and
 * a repo gets a line of its own only when something about it failed.
 *
 * `initial` must be a read taken just before this call: it decides which
 * repos are sent to the grant page and whether there is anything to wait for.
 */
export async function connectRepos(token: string, repos: string[], initial: ResolvedRepos, options: ConnectOptions): Promise<ResolvedRepos> {
  const urls = workspaceUrls(initial.workspace.slug);
  const poll = options.poll ?? ((signal?: AbortSignal) => resolveRepos(token, repos, fetch, signal));
  const project = options.project;
  const label = options.label ?? ((slug: string) => slug);
  const movable = options.movable ?? new Set<string>();
  const announce = options.announce ?? new Set<string>();
  const track = options.track ?? plainTrack(options.say);
  const assign =
    options.assign ??
    ((names: string[], signal?: AbortSignal) => assignRepos(token, project ?? "", names, fetch, signal));
  // A project this run has not seen in the workspace may not exist at all, so
  // there is nothing to place repos into and the browser owns both steps.
  let placeable = project !== undefined && options.projectExists === true;
  let saidBrowserAssign = false;
  // What happened to a repo that failed, said once each. During the wait the
  // status line owns the terminal, so these are held until it ends.
  const said = new Set<string>();
  let held: string[] | null = null;
  const report = (line: string): void => {
    if (said.has(line)) return;
    said.add(line);
    if (held !== null) held.push(line);
    else options.say(line);
  };

  const browserAssign = (): void => {
    if (saidBrowserAssign) return;
    saidBrowserAssign = true;
    report(`Assign the requested repos: ${urls.repos}`);
    report(`  Select ${repos.join(", ")}, choose the target project in "Move selected to", then choose "Move selected".`);
  };

  /** Place what can be placed; true when something moved and is worth re-reading. */
  const place = async (identity: ResolvedRepos, signal?: AbortSignal): Promise<boolean> => {
    if (!placeable || project === undefined) return false;
    const pending = misplaced(identity, repos, project, movable);
    if (pending.length === 0) return false;
    const outcome = await assign(pending, signal);
    if (outcome.kind !== "placed") {
      // Neither a refusal nor an absence is worth retrying every five seconds,
      // and both leave the assignment to the browser.
      placeable = false;
      if (outcome.kind === "refused") {
        report(`Carrick did not assign the requested repos to ${label(project)}: ${outcome.message}`);
      }
      return false;
    }
    let moved = false;
    for (const repo of outcome.repos) {
      if (!repo.assigned) {
        report(`${repo.full_name} was not moved: ${repo.reason ?? "Carrick gave no reason."}`);
        continue;
      }
      moved = moved || repo.moved;
      const row = identity.repos.find(
        (candidate) => candidate.full_name.toLowerCase() === repo.full_name.toLowerCase(),
      );
      const from = row?.connected === true ? row.project_slug : null;
      if (repo.moved && from && announce.has(repo.full_name.toLowerCase())) {
        report(`Moved ${repo.full_name} from ${label(from)} into ${label(project)}.`);
      }
    }
    return moved;
  };

  /** Place, then read the assignment back: a claim is only made on the read. */
  const settle = async (identity: ResolvedRepos, signal?: AbortSignal): Promise<ResolvedRepos> => {
    if (!(await place(identity, signal))) return identity;
    return poll(signal);
  };

  /**
   * Whether a person has to move a repo on the Repos page: this run cannot
   * place repos at all, or a connected repo sits in another project and
   * nobody consented to moving it (carrick#1338).
   */
  const byHand = (identity: ResolvedRepos): boolean =>
    project !== undefined &&
    (!placeable ||
      identity.repos.some(
        (repo) =>
          repo.connected &&
          repo.project_slug !== project &&
          repos.some((name) => name.toLowerCase() === repo.full_name.toLowerCase()) &&
          !movable.has(repo.full_name.toLowerCase()),
      ));

  const settled = (identity: ResolvedRepos): boolean =>
    project !== undefined
      ? reposAreInProject(identity, repos, project)
      : identity.workspace.installed && identity.repos.every((repo) => repo.connected);

  let latest = initial;
  if (project !== undefined) {
    if (settled(latest)) return latest;
    latest = await settle(latest, options.signal);
    if (settled(latest)) return latest;
    // The create step is skipped only when this run has just seen the project
    // in the workspace or created it.
    if (!options.projectExists) {
      options.say(`Create project "${project}" if needed: ${urls.projects}`);
      options.say(`  Choose Create project, enter a name, and set the slug to "${project}".`);
    }
  } else if (settled(latest)) {
    return latest;
  }
  // The grant itself is not said here: the list the reader said yes to named
  // it, with its link where there is no terminal to open it from, and a
  // second line saying the same thing was the repeat in carrick#1512.
  const unconnected = unconnectedRepos(latest, repos);
  // Only where this run cannot do the placing itself.
  if (byHand(latest)) browserAssign();
  if (!options.interactive) {
    if (project !== undefined) {
      options.say(`Project ${label(project)} is not verified for every requested repo.`);
    }
    return latest;
  }
  // Where the browser is pointed: at the project it must create, else at the
  // grant — only where a repo really is unconnected — else, only where this
  // run cannot place them itself, at the page that moves them.
  const target =
    project !== undefined && !options.projectExists
      ? urls.projects
      : unconnected.length > 0
        ? urls.connect
        : byHand(latest)
          ? urls.repos
          : null;
  // Who can finish a browser step, said only where there is one — a wait on
  // placements this run makes itself has nothing for an owner to do — and
  // only to a member, who is the one it is news to (carrick#1512).
  if (target !== null) {
    if (options.member === true) options.say(adminWait(options.again));
    // The page is named only where it could not be opened: a reader looking
    // at it in the browser does not need its address as well.
    const opened = await (options.open ?? openBrowser)(target).catch(() => false);
    if (!opened) options.say(`Open ${target} in your browser.`);
  }

  /** Where the wait stands, as one line. */
  const status = (identity: ResolvedRepos): string => {
    const missing = unconnectedRepos(identity, repos).length;
    if (missing > 0) {
      return `Waiting for GitHub: ${repos.length - missing} of ${repoCount(repos.length)} connected`;
    }
    if (project === undefined) return `Waiting for GitHub: ${repoCount(repos.length)} connected`;
    // The move instruction is held with the other lines while this shows, so
    // the page it names is on the status line itself.
    return `Waiting for ${repoCount(repos.length)} to reach ${label(project)}${byHand(identity) ? `: move them at ${urls.repos}` : ""}`;
  };

  const controller = new AbortController();
  const cancel = (): void => controller.abort();
  process.once("SIGINT", cancel);
  const signal = AbortSignal.any([controller.signal, AbortSignal.timeout(30 * 60_000), ...(options.signal ? [options.signal] : [])]);
  held = [];
  let done = false;
  try {
    await track(
      status(latest),
      async (progress) => {
        let shown = status(latest);
        progress(`${shown}. Press Ctrl-C to stop.`);
        try {
          while (!signal.aborted) {
            await (options.wait ?? (async (signal) => { await setTimeout(5000, undefined, { signal }); }))(signal);
            signal.throwIfAborted();
            latest = await settle(await poll(signal), signal);
            if (settled(latest)) {
              done = true;
              return;
            }
            if (byHand(latest)) browserAssign();
            const now = status(latest);
            if (now !== shown) {
              shown = now;
              progress(`${now}. Press Ctrl-C to stop.`);
            }
          }
        } catch (error) {
          if (!signal.aborted) throw error;
        }
      },
      () =>
        done
          ? {
              kind: "done",
              // The count alone: the line after the wait names the project
              // and its repos (carrick#1512).
              text: `${repoCount(repos.length)} connected`,
            }
          : {
              kind: "warn",
              text:
                project !== undefined
                  ? `Stopped waiting; project ${label(project)} is not verified for every requested repo.`
                  : "Stopped waiting for repository connections; continuing local setup.",
            },
    );
  } finally {
    process.removeListener("SIGINT", cancel);
    const lines = held;
    held = null;
    for (const line of lines) options.say(line);
  }
  return latest;
}
