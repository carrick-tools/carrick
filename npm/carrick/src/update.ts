// Telling a machine that the Carrick it is running is not the Carrick that is
// published.
//
// `npx --yes carrick@latest` resolves fresh on every invocation, so it is never
// the problem. An INSTALLED `carrick` is: `npm install -g carrick` is what the
// README, the plugin docs and `carrick init` all tell people to run, and from
// that moment the version on that machine is frozen until someone remembers to
// bump it. A pilot customer ran a scan on a build three releases old and it
// failed on a defect that was already fixed; the same morning two versions were
// live on one machine, which is what a stale global install beside a fresh npx
// looks like.
//
// The design this file implements, and why, is
// `docs/reference/update-check.md`.
//
// Four properties it must not lose:
//
//  1. **Fail open.** No network, a 500, a rate limit, a corrupt cache, an
//     unwritable config directory — every one of them yields no output and
//     changes nothing. A scan must never fail because a registry did.
//  2. **No latency.** On a laptop the invocation reads one small file and
//     prints from it; the fetch happens in a detached child whose answer the
//     NEXT invocation reads. One run's lag after a release, and zero
//     milliseconds on the path a person waits on.
//  3. **It never changes what runs.** The notice names the exact command for
//     the install shape it detected. Nothing here installs anything. The
//     reasoning is in the doc: we do not own the install location (npm global,
//     pnpm, bun, volta, an npx cache, a devDependency), and a self-update that
//     guesses wrong leaves two Carricks on one machine — which is the incident
//     this exists to prevent, not a fix for it.
//  4. **Suppressible.** `CARRICK_NO_UPDATE_CHECK=1` turns the whole thing off,
//     for a reproducible run or an air-gapped machine.

import fs from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

import { configDir } from "./auth/credentials.ts";
import { packageRoot } from "./native.ts";

/** The registry's smallest answer to "what is published": a two-key document. */
export const DIST_TAGS_URL = "https://registry.npmjs.org/-/package/carrick/dist-tags";

/**
 * How long an answer stands before another is fetched.
 *
 * Short enough that a morning's work does not run on yesterday's news — the
 * incident was a customer on a three-release-old build for a whole morning —
 * and long enough that a repository of hooks firing on every edit does not
 * become registry traffic.
 */
export const CHECK_TTL_MS = 4 * 60 * 60 * 1000;

/** The whole budget for a fetch, on the one path where it is synchronous. */
export const FETCH_TIMEOUT_MS = 2000;

/** What the last check learned. `latest` is null when it never got an answer. */
export type UpdateState = {
  checked_at: string;
  latest: string | null;
};

export function updateStatePath(env: NodeJS.ProcessEnv = process.env): string {
  return path.join(configDir(env), "update-check.json");
}

/** True when the user has asked for no version checking at all. */
export function suppressed(env: NodeJS.ProcessEnv = process.env): boolean {
  const raw = env["CARRICK_NO_UPDATE_CHECK"];
  return raw !== undefined && raw !== "" && raw !== "0" && raw !== "false";
}

/**
 * True on a build machine.
 *
 * `CI` is the convention every provider sets; `GITHUB_ACTIONS` additionally
 * decides how a notice is spelled. CI never gets the background check: the
 * machine is thrown away at the end of the job, so a cache written for the next
 * invocation is a cache nobody reads.
 */
export function inCi(env: NodeJS.ProcessEnv = process.env): boolean {
  return env["CI"] === "true" || env["CI"] === "1" || Boolean(env["GITHUB_ACTIONS"]);
}

/** This package's own version, which is the version a user is running. */
export function currentVersion(root: string = packageRoot()): string | null {
  try {
    const manifest = JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8"));
    return typeof manifest.version === "string" ? manifest.version : null;
  } catch {
    return null;
  }
}

/**
 * Whether `candidate` is a release a user should move to.
 *
 * Deliberately narrow: both sides must be a plain `x.y.z`, so a prerelease on
 * either side answers false and nobody is ever nagged towards one. The
 * registry's `latest` tag is a release by definition, and a scanner built from
 * a checkout carries whatever `Cargo.toml` said, so the guard costs nothing and
 * removes a whole class of wrong notice.
 */
export function isNewer(candidate: string, current: string): boolean {
  const parse = (value: string): number[] | null => {
    const match = /^(\d+)\.(\d+)\.(\d+)$/.exec(value.trim());
    return match ? [Number(match[1]), Number(match[2]), Number(match[3])] : null;
  };
  const a = parse(candidate);
  const b = parse(current);
  if (!a || !b) return false;
  for (let index = 0; index < 3; index += 1) {
    if (a[index]! > b[index]!) return true;
    if (a[index]! < b[index]!) return false;
  }
  return false;
}

export function readUpdateState(env: NodeJS.ProcessEnv = process.env): UpdateState | null {
  try {
    const parsed = JSON.parse(fs.readFileSync(updateStatePath(env), "utf8"));
    if (typeof parsed?.checked_at !== "string") return null;
    const latest = typeof parsed.latest === "string" ? parsed.latest : null;
    return { checked_at: parsed.checked_at, latest };
  } catch {
    // Absent, unreadable, or not the shape this build writes. All three mean
    // "nothing is known", which is a state this file already handles.
    return null;
  }
}

/**
 * Replace the cache atomically, or do nothing.
 *
 * Returns whether the write landed, because the caller uses a failed write as
 * its reason not to spawn a checker: a machine whose config directory cannot be
 * written would otherwise fork a child on every single invocation forever.
 */
export function writeUpdateState(state: UpdateState, env: NodeJS.ProcessEnv = process.env): boolean {
  const target = updateStatePath(env);
  const temporary = `${target}.${process.pid}.tmp`;
  try {
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.writeFileSync(temporary, `${JSON.stringify(state)}\n`, { mode: 0o600 });
    fs.renameSync(temporary, target);
    return true;
  } catch {
    try {
      fs.unlinkSync(temporary);
    } catch {
      // The temporary file is the one thing here worth tidying, and failing to
      // tidy it is still not a reason to say anything to anybody.
    }
    return false;
  }
}

export function isStale(state: UpdateState | null, now: number = Date.now()): boolean {
  if (!state) return true;
  const at = Date.parse(state.checked_at);
  if (!Number.isFinite(at)) return true;
  // A clock that moved backwards (a VM restored, a timezone fixed) reads as
  // stale rather than as "checked in the future, wait four hours".
  return now - at >= CHECK_TTL_MS || at > now;
}

/** How this copy of the package got onto the machine, and the way out of it. */
export type InstallShape = {
  kind: "npx" | "volta" | "bun" | "pnpm" | "project" | "npm";
  /** A command a person or an agent can run verbatim. */
  command: string;
  /** The manifest that pins it, when one does. Named so the fix is findable. */
  manifest?: string;
};

function readJson(file: string): Record<string, unknown> | null {
  try {
    const parsed = JSON.parse(fs.readFileSync(file, "utf8"));
    return parsed && typeof parsed === "object" ? parsed : null;
  } catch {
    return null;
  }
}

function dependsOnCarrick(manifest: Record<string, unknown>): boolean {
  for (const field of ["dependencies", "devDependencies", "optionalDependencies"]) {
    const block = manifest[field];
    if (block && typeof block === "object" && "carrick" in block) return true;
  }
  return false;
}

/**
 * The install shape, read off the path this file was loaded from.
 *
 * Nothing here runs a package manager or shells out: it is string work on an
 * absolute path plus at most two small file reads, so it costs nothing and
 * cannot hang. Every branch ends in a runnable command, and the ordering
 * matters — a pnpm project install and a pnpm global install both sit under
 * `.pnpm`, and only the enclosing manifest tells them apart.
 */
export function installShape(
  root: string = packageRoot(),
  env: NodeJS.ProcessEnv = process.env,
): InstallShape {
  const normalized = root.split(path.sep).join("/");
  const segments = `/${normalized}/`;

  // npx keeps each resolved tree under a content-addressed directory and reuses
  // it, so a bare `npx carrick` is as frozen as a global install. `@latest` is
  // the whole fix, and it is the fix for the next invocation too.
  if (segments.includes("/_npx/")) {
    return { kind: "npx", command: "npx --yes carrick@latest" };
  }
  if (segments.includes("/.volta/")) {
    return { kind: "volta", command: "volta install carrick@latest" };
  }
  if (segments.includes("/.bun/")) {
    return { kind: "bun", command: "bun add -g carrick@latest" };
  }
  // pnpm's global root is a real project directory with a real manifest that
  // lists what was installed into it, so it has to be recognised before the
  // project branch below would claim it.
  if (segments.includes("/pnpm/global/")) {
    return { kind: "pnpm", command: "pnpm add -g carrick@latest" };
  }

  // Everything below the OUTERMOST `node_modules` belongs to whatever encloses
  // it. For a project install that is the repository; for `npm -g` it is the
  // prefix directory, which has no manifest, so the project branch declines and
  // the global default answers.
  const outer = normalized.split("/node_modules/")[0];
  if (outer && outer !== normalized) {
    const manifestPath = path.join(outer.split("/").join(path.sep), "package.json");
    const manifest = readJson(manifestPath);
    if (manifest && dependsOnCarrick(manifest)) {
      const has = (name: string): boolean =>
        fs.existsSync(path.join(outer.split("/").join(path.sep), name));
      const command = has("pnpm-lock.yaml")
        ? "pnpm add -D carrick@latest"
        : has("yarn.lock")
          ? "yarn add -D carrick@latest"
          : has("bun.lockb") || has("bun.lock")
            ? "bun add -d carrick@latest"
            : "npm install -D carrick@latest";
      return { kind: "project", command, manifest: manifestPath };
    }
  }

  void env;
  return { kind: "npm", command: "npm install -g carrick@latest" };
}

export type NoticeOptions = {
  env?: NodeJS.ProcessEnv;
  root?: string;
};

/**
 * The one line a user sees, or null when there is nothing to say.
 *
 * It carries three things on purpose: the version running, the version
 * published, and a command. The first two are the reporting half — a bug report
 * that quotes this line names the build it came from — and the third is the
 * only reason anybody acts on it. On GitHub Actions it is spelled as a workflow
 * warning so it lands in the run's annotations rather than scrolling past in a
 * multi-minute log, and it says plainly that the run continues on the old
 * version: CI reproducibility means the workflow decides what runs, not us.
 */
export function updateNotice(
  current: string | null,
  latest: string | null,
  options: NoticeOptions = {},
): string | null {
  const env = options.env ?? process.env;
  if (!current || !latest) return null;
  if (!isNewer(latest, current)) return null;
  if (env["GITHUB_ACTIONS"]) {
    const head =
      `::warning::Carrick ${current} is running here and ${latest} is published. ` +
      `This run continues on ${current} — nothing was changed. `;
    // Two different fixes, and naming the wrong one sends a reader to a line
    // their workflow does not contain. The runner sets
    // GITHUB_ACTION_REPOSITORY for a step that belongs to an action, so a run
    // through the Carrick action says so: there, the version comes from the
    // action checkout's own Cargo.toml, and the ref is the only thing the
    // workflow chose. A workflow that calls the CLI directly wrote a version
    // string, and that is what it has to change.
    if ((env["GITHUB_ACTION_REPOSITORY"] ?? "").endsWith("/carrick")) {
      return `${head}This workflow pins the Carrick action to a ref that does not move; use \`carrick-tools/carrick@v1\`, which moves to each release.`;
    }
    return `${head}Run \`carrick@latest\`, or ${latest}, so the next run is not on a build with defects already fixed.`;
  }
  const shape = installShape(options.root ?? packageRoot(), env);
  const where = shape.manifest ? ` (pinned in ${shape.manifest})` : "";
  // No mention of CARRICK_NO_UPDATE_CHECK here. This line is read by agents as
  // well as people — the session-start hook puts it on stdout, straight into
  // the session — and the failure this exists for is an agent that does not
  // upgrade. Handing it an off switch in the same sentence as the fix invites
  // the wrong one. The variable is documented in `carrick --help` and in the
  // package README, where a person reads it.
  return (
    `carrick ${current} is installed${where} and ${latest} is published. ` +
    `Update with \`${shape.command}\` — a scan on an older build can fail on defects that are already fixed.`
  );
}

/** One bounded GET of the registry's dist-tags, answering null on anything at all. */
export async function fetchLatest(
  fetchImpl: typeof fetch = fetch,
  timeoutMs: number = FETCH_TIMEOUT_MS,
): Promise<string | null> {
  try {
    const response = await fetchImpl(DIST_TAGS_URL, {
      signal: AbortSignal.timeout(timeoutMs),
      headers: { accept: "application/json" },
    });
    if (!response.ok) return null;
    const body = (await response.json()) as Record<string, unknown>;
    const latest = body?.["latest"];
    return typeof latest === "string" && /^\d+\.\d+\.\d+$/.test(latest) ? latest : null;
  } catch {
    return null;
  }
}

/**
 * Start a detached child to refresh the cache, if the cache is due a refresh.
 *
 * The parent stamps the cache with the current time BEFORE spawning, keeping
 * whatever `latest` it already knew. That stamp is what makes a dead network
 * cheap: the child can fail silently and no further invocation forks another
 * one until the TTL comes round again. A stamp that cannot be written is the
 * signal to spawn nothing at all.
 *
 * Returns whether a child was started, for the tests and for the log.
 */
export function scheduleUpdateCheck(
  env: NodeJS.ProcessEnv = process.env,
  spawnImpl: typeof spawn = spawn,
): boolean {
  if (suppressed(env) || inCi(env)) return false;
  const state = readUpdateState(env);
  if (!isStale(state)) return false;
  if (!writeUpdateState({ checked_at: new Date().toISOString(), latest: state?.latest ?? null }, env)) {
    return false;
  }
  try {
    const child = spawnImpl(
      process.execPath,
      [fileURLToPath(new URL("./update-check.js", import.meta.url))],
      // `windowsHide` because a detached child on Windows gets its own console
      // window otherwise, and win32-x64 is a platform this package publishes:
      // a window flashing up once every four hours is not a version notice.
      { detached: true, stdio: "ignore", env, windowsHide: true },
    );
    child.unref();
    return true;
  } catch {
    return false;
  }
}

/**
 * The laptop path: print what the last check learned, then start the next one.
 *
 * Nothing is awaited, so the cost to the command is one file read and one
 * `spawn` at most once every four hours.
 */
export function updateNoticeFromCache(env: NodeJS.ProcessEnv = process.env): string | null {
  if (suppressed(env)) return null;
  const state = readUpdateState(env);
  const notice = updateNotice(currentVersion(), state?.latest ?? null, { env });
  scheduleUpdateCheck(env);
  return notice;
}

/**
 * Every command name this CLI answers: the binary's own
 * (`src/local_mode/cli.rs`, `LOCAL_COMMANDS`) and this package's.
 *
 * Needed only to tell a command from a path. The scanner reads a first
 * argument it does not recognise as a directory to scan, and that is the
 * invocation this list exists to identify.
 */
const COMMANDS = new Set([
  // The binary's.
  "derive",
  "index",
  "refresh",
  "resume",
  "status",
  "check",
  "touch",
  // This package's.
  "login",
  "logout",
  "lsp",
  "hook",
  "init",
  "remove",
  "doctor",
  "templates",
]);

/** The commands that pay for a model and take minutes. */
const SCAN_COMMANDS = new Set(["index"]);

/**
 * Whether this invocation is a scan.
 *
 * Only used to decide whether CI pays for a synchronous check: a two-second
 * bound in front of a multi-minute scan is invisible, and in front of
 * `carrick status` it is the whole command.
 */
export function isScanInvocation(argv: string[]): boolean {
  const first = argv[0];
  if (first === undefined) return false;
  if (first.startsWith("-")) return false;
  if (SCAN_COMMANDS.has(first)) return true;
  return !COMMANDS.has(first);
}

/**
 * The CI path: one bounded fetch, in front of a scan only, warning and
 * continuing.
 *
 * A build machine is thrown away at the end of the job, so the cached model a
 * laptop uses would never get a second invocation to read it. This is the one
 * place a check is on the critical path, and it is bounded at two seconds
 * before work that takes minutes. It never refuses the run: CI's contract is
 * that the workflow decides what runs, and a scanner that declined to start
 * because a newer one exists would be a worse outage than the stale build.
 */
export async function ciUpdateNotice(
  argv: string[],
  env: NodeJS.ProcessEnv = process.env,
  fetchImpl: typeof fetch = fetch,
): Promise<string | null> {
  if (suppressed(env) || !inCi(env) || !isScanInvocation(argv)) return null;
  const latest = await fetchLatest(fetchImpl);
  return updateNotice(currentVersion(), latest, { env });
}
