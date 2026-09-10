import { setTimeout } from "node:timers/promises";
import { APP_BASE } from "../auth/credentials.ts";
import { openBrowser } from "../auth/oauth.ts";
import { resolveRepos, type ResolvedRepos } from "../auth/read.ts";

type ConnectOptions = {
  interactive: boolean;
  say: (message: string) => void;
  open?: (url: string) => Promise<boolean>;
  poll?: (signal: AbortSignal) => Promise<ResolvedRepos>;
  wait?: (signal: AbortSignal) => Promise<void>;
  signal?: AbortSignal;
};

/** The browser owns installation. Ctrl-C stops waiting and lets setup continue. */
export async function connectRepos(token: string, repos: string[], initial: ResolvedRepos, options: ConnectOptions): Promise<ResolvedRepos> {
  if (initial.workspace.installed && initial.repos.every((repo) => repo.connected)) return initial;
  const url = `${APP_BASE}/w/${encodeURIComponent(initial.workspace.slug)}/connect`;
  options.say(`Connect repositories in your browser: ${url}`);
  if (!options.interactive) return initial;
  options.say("Waiting for repository connections. Press Ctrl-C to continue setup without waiting.");
  const controller = new AbortController();
  const cancel = (): void => controller.abort();
  process.once("SIGINT", cancel);
  const signal = AbortSignal.any([controller.signal, AbortSignal.timeout(30 * 60_000), ...(options.signal ? [options.signal] : [])]);
  let latest = initial;
  const seen = new Set(initial.repos.filter((repo) => repo.connected).map((repo) => repo.full_name));
  try {
    void (options.open ?? openBrowser)(url).catch(() => false);
    while (!signal.aborted) {
      await (options.wait ?? (async (signal) => { await setTimeout(5000, undefined, { signal }); }))(signal);
      signal.throwIfAborted();
      latest = await (options.poll ?? ((signal) => resolveRepos(token, repos, fetch, signal)))(signal);
      for (const repo of latest.repos) {
        if (repo.connected && !seen.has(repo.full_name)) {
          options.say(`Connected ${repo.full_name}.`);
          seen.add(repo.full_name);
        }
      }
      if (latest.workspace.installed && latest.repos.every((repo) => repo.connected)) return latest;
    }
  } catch (error) { if (!signal.aborted) throw error; }
  finally { process.removeListener("SIGINT", cancel); }
  options.say("Stopped waiting for repository connections; continuing local setup.");
  return latest;
}
