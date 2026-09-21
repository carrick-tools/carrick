// Keeping the `carrick` a PATH lookup answers with level with the one this run
// is (carrick#1372).
//
// `npx carrick@latest` runs the published version out of a cache directory and
// changes nothing else on the machine. A global install from three releases ago
// stays exactly where it was, and it is the one that answers when an agent hook
// runs `carrick hook post-edit`, when a script types `carrick index`, and when a
// person opens a new shell. That is the "two versions on one machine in one
// morning" incident: the run a human watched was current and everything that
// ran afterwards was not.
//
// `src/update.ts` is the other half of this and does the opposite thing on
// purpose: it NOTIFIES, because it cannot know which of several possible
// installs a notice is about. This file only ever acts on an install that
// already exists, through the package manager that owns it, and never creates
// one without being told to. The four properties:
//
//  1. **Only an upgrade, only of what is there.** An older global is brought
//     level with the running version. A global that is current, newer, or
//     absent is left alone, and nothing is ever installed on a machine that has
//     no global without a yes or `--install-global`.
//  2. **The owning manager, with its own configuration.** The command is the
//     one for the shape the install is in (npm, pnpm, bun, volta). No flag of
//     ours overrides a registry, a prefix or a release-age policy: a machine
//     whose policy refuses this version is a machine that gets told, not one
//     that gets overridden. A project `.npmrc` is not read in global mode
//     (proved in `docs/reference/update-check.md`); a user-level one is, and
//     its refusal is what the reader sees.
//  3. **It verifies.** After the install it resolves `carrick` again. A second
//     install earlier on PATH, a version-manager shim, or an install that
//     reported success and changed nothing all end with a line naming the path
//     that is still stale and the command for it. The run never ends silently
//     on an older binary.
//  4. **It never fails the command.** Every refusal, timeout and unreadable
//     version prints and carries on. `CARRICK_NO_UPDATE_CHECK=1` turns it off
//     along with the notice.

import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

import { packageRoot } from "./native.ts";
import { currentVersion, inCi, installShape, isNewer, suppressed } from "./update.ts";
import type { InstallShape } from "./update.ts";

/** The names a `carrick` on PATH goes by, most specific first. */
export function carrickNames(platform: string = process.platform): string[] {
  return platform === "win32" ? ["carrick.cmd", "carrick.exe", "carrick"] : ["carrick"];
}

/** A `carrick` that is installed on this machine and is not the one running. */
export type GlobalCarrick = {
  /** What the PATH lookup answers with. */
  binary: string;
  /** Where that leads after symlinks: an npm global bin entry is a link. */
  real: string;
  /** Its version, or null when neither its manifest nor the binary said. */
  version: string | null;
};

export type FindOptions = {
  env?: NodeJS.ProcessEnv;
  platform?: string;
  exists?: (target: string) => boolean;
  real?: (target: string) => string;
  /** This package's own root, so the copy doing the looking is skipped. */
  ours?: string;
};

function defaultExists(target: string): boolean {
  try {
    return fs.existsSync(target);
  } catch {
    return false;
  }
}

function defaultReal(target: string): string {
  try {
    return fs.realpathSync(target);
  } catch {
    return target;
  }
}

function inside(target: string, directory: string): boolean {
  const slashed = (value: string): string => `${value.split(path.sep).join("/")}/`;
  return slashed(target).startsWith(slashed(directory));
}

/** npx resolves a package into a cache tree and puts its `.bin` first on PATH. */
function fromNpxCache(target: string): boolean {
  return `/${target.split(path.sep).join("/")}/`.includes("/_npx/");
}

/**
 * The `carrick` a PATH lookup finds, skipping the one doing the looking.
 *
 * `which carrick` cannot answer this question. npm puts the exec tree's own
 * `node_modules/.bin` at the front of PATH for the child it runs, so inside
 * `npx carrick init` a `which` says `carrick` is on PATH on a machine where it
 * is not — and `carrick init` then wrote a bare `carrick hook post-edit` into a
 * settings file, naming a command that stopped existing when npx exited. So the
 * PATH is walked here, and an entry that leads back into this package or into an
 * npx cache is not a global.
 *
 * File reads only: no `which`, no subprocess, so it costs nothing on a command
 * that runs on every edit.
 */
export function findCarrick(options: FindOptions = {}): GlobalCarrick | null {
  const env = options.env ?? process.env;
  const exists = options.exists ?? defaultExists;
  const real = options.real ?? defaultReal;
  const ours = options.ours ?? packageRoot();
  const raw = env["PATH"] ?? env["Path"] ?? "";
  for (const directory of raw.split(path.delimiter)) {
    if (directory === "") continue;
    for (const name of carrickNames(options.platform)) {
      const binary = path.join(directory, name);
      if (!exists(binary)) continue;
      const resolved = real(binary);
      if (fromNpxCache(binary) || fromNpxCache(resolved)) continue;
      if (inside(resolved, ours) || inside(binary, ours)) continue;
      return { binary, real: resolved, version: null };
    }
  }
  return null;
}

export type VersionOptions = {
  env?: NodeJS.ProcessEnv;
  platform?: string;
  read?: (target: string) => string;
  run?: (binary: string) => string | null;
};

/** How far up from a binary a `package.json` of ours can be. */
const MANIFEST_DEPTH = 5;

/**
 * The version of the install a binary belongs to.
 *
 * Read off the manifest above it wherever the path says — an npm global bin
 * entry links straight into `lib/node_modules/carrick/bin`, so the answer is a
 * file read. A shim that is a real file (volta, a `.cmd` on Windows) leads
 * nowhere, and only then is the binary asked, with the version check turned off
 * in the child so an older build does not fork a registry fetch from our probe.
 */
export function installedVersion(found: GlobalCarrick, options: VersionOptions = {}): string | null {
  const read = options.read ?? ((target: string) => fs.readFileSync(target, "utf8"));
  let directory = path.dirname(found.real);
  for (let step = 0; step < MANIFEST_DEPTH; step += 1) {
    try {
      const manifest = JSON.parse(read(path.join(directory, "package.json")));
      if (manifest?.name === "carrick" && typeof manifest.version === "string") return manifest.version;
    } catch {
      // Not there, not readable, or not ours. Keep walking up.
    }
    const parent = path.dirname(directory);
    if (parent === directory) break;
    directory = parent;
  }
  const run = options.run ?? defaultAskVersion(options);
  return run(found.binary);
}

function defaultAskVersion(options: VersionOptions): (binary: string) => string | null {
  const env = options.env ?? process.env;
  const platform = options.platform ?? process.platform;
  return (binary) => {
    const answer = spawnSync(binary, ["--version"], {
      encoding: "utf8",
      timeout: 5000,
      shell: platform === "win32",
      env: { ...env, CARRICK_NO_UPDATE_CHECK: "1" },
    });
    if (answer.status !== 0 || typeof answer.stdout !== "string") return null;
    const first = answer.stdout.trim().split("\n")[0]?.trim();
    return first && /^\d+\.\d+\.\d+$/.test(first) ? first : null;
  };
}

/**
 * The command that replaces a global install, for the manager that owns it.
 *
 * Null for the two shapes that are not a global: an npx cache directory, which
 * is thrown away and re-resolved, and a project dependency, which belongs to
 * whichever repository pins it and is not this run's to change. Both are
 * reported rather than acted on.
 *
 * The version is pinned rather than `@latest` on purpose. The run knows which
 * version it is, `@latest` is a second question for the registry, and a pinned
 * spec is what makes the verification below a comparison rather than a guess.
 */
export function globalCommand(kind: InstallShape["kind"], version: string): string[] | null {
  switch (kind) {
    case "npm":
      return ["npm", "install", "-g", `carrick@${version}`];
    case "pnpm":
      return ["pnpm", "add", "-g", `carrick@${version}`];
    case "bun":
      return ["bun", "add", "-g", `carrick@${version}`];
    case "volta":
      return ["volta", "install", `carrick@${version}`];
    default:
      return null;
  }
}

/**
 * The command for a machine that has no global carrick at all.
 *
 * The running copy's own shape names the manager: somebody running through
 * `npx` has npm, and a project dependency says nothing about what its owner
 * would want globally, so both answer npm.
 */
export function firstGlobalCommand(kind: InstallShape["kind"], version: string): string[] {
  return globalCommand(kind === "npx" || kind === "project" ? "npm" : kind, version)!;
}

export type InstallResult = { ok: boolean; reason: string | null };

/** What this file needs from the machine, so every case above can be a test. */
export type GlobalMachine = {
  /** The global carrick on PATH with its version, or null where there is none. */
  find: () => GlobalCarrick | null;
  /** The shape an installed path is in, for the command that replaces it. */
  shape: (found: GlobalCarrick) => InstallShape;
  /** Run an install. `ok` is the manager's own verdict, nothing more. */
  install: (argv: string[]) => InstallResult;
};

/** How long an install is given before it is a refusal with a reason. */
export const INSTALL_TIMEOUT_MS = 180_000;

/** The last thing a failed install said, for a reader who has to act on it. */
export function failureReason(result: {
  status: number | null;
  stderr: string | null;
  stdout: string | null;
  error?: Error;
}): string {
  if (result.error) return result.error.message;
  const said = `${result.stderr ?? ""}\n${result.stdout ?? ""}`
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "" && !line.startsWith("npm warn"));
  const last = said[said.length - 1];
  return last ?? `it exited ${result.status ?? "without a status"}`;
}

export function realMachine(env: NodeJS.ProcessEnv = process.env): GlobalMachine {
  return {
    find: () => {
      const found = findCarrick({ env });
      if (!found) return null;
      return { ...found, version: installedVersion(found, { env }) };
    },
    shape: (found) => installShape(found.real, env),
    install: (argv) => {
      const answer = spawnSync(argv[0]!, argv.slice(1), {
        encoding: "utf8",
        timeout: INSTALL_TIMEOUT_MS,
        // A manager on Windows is a `.cmd`, which Node will not run without
        // one. Every word of this command is built here from a fixed list and
        // a version this package's own manifest states.
        shell: process.platform === "win32",
        stdio: ["ignore", "pipe", "pipe"],
        env,
      });
      if (answer.status === 0) return { ok: true, reason: null };
      return {
        ok: false,
        reason: failureReason({
          status: answer.status,
          stderr: answer.stderr ?? null,
          stdout: answer.stdout ?? null,
          error: answer.error ?? undefined,
        }),
      };
    },
  };
}

/** What a run did about the global install, for the tests and for the log. */
export type SyncOutcome =
  | { kind: "skipped"; why: string }
  | { kind: "current"; version: string }
  | { kind: "upgraded"; from: string; to: string }
  | { kind: "refused"; from: string; to: string; reason: string; command: string }
  | { kind: "stale"; path: string | null; version: string | null; command: string | null };

export type SyncOptions = {
  /** The version this run is. */
  running?: string | null;
  /** One line to the reader, before the install and after it. */
  say: (line: string) => void;
  machine?: GlobalMachine;
  env?: NodeJS.ProcessEnv;
};

/**
 * Bring an older global carrick level with this run, and say so.
 *
 * The ruling this implements: somebody who installed carrick globally has
 * already chosen to have it on this machine, so bringing that choice up to date
 * is inside what they agreed to and is not asked about. What is NOT inside it
 * is creating an install they never asked for, forcing one past their machine's
 * policy, or leaving them on an older binary without knowing.
 */
export function syncGlobalInstall(options: SyncOptions): SyncOutcome {
  const env = options.env ?? process.env;
  const machine = options.machine ?? realMachine(env);
  const running = options.running === undefined ? currentVersion() : options.running;
  if (!running || !/^\d+\.\d+\.\d+$/.test(running)) {
    return { kind: "skipped", why: "this build does not state a plain version" };
  }
  const found = machine.find();
  if (!found) return { kind: "skipped", why: "no carrick on PATH" };
  if (found.version === null) {
    return { kind: "skipped", why: `${found.binary} does not say which version it is` };
  }
  if (!isNewer(running, found.version)) return { kind: "current", version: found.version };

  const shape = machine.shape(found);
  const argv = globalCommand(shape.kind, running);
  if (!argv) {
    // A project dependency or an npx cache answering `carrick`. Neither is this
    // run's to replace, and both leave the reader on the older one.
    const where = shape.manifest ? ` (pinned in ${shape.manifest})` : "";
    options.say(
      `\`carrick\` here runs ${found.version} from ${found.binary}${where}, not the ${running} this run is. Update it where it is pinned, or put a global install earlier on PATH.`,
    );
    return { kind: "stale", path: found.binary, version: found.version, command: shape.command };
  }

  const command = argv.join(" ");
  options.say(`Upgrading the global carrick ${found.version} -> ${running} (${command})`);
  const result = machine.install(argv);
  if (!result.ok) {
    options.say(
      `The global carrick is still ${found.version}: ${result.reason ?? "the install did not finish"}. Run \`${command}\` when that is sorted.`,
    );
    return { kind: "refused", from: found.version, to: running, reason: result.reason ?? "", command };
  }

  // The install said it worked, which is not the same as `carrick` now being
  // the new one: a second install earlier on PATH, or a version manager's shim,
  // answers first and was never touched by it.
  const after = machine.find();
  if (!after) {
    options.say(
      `carrick ${running} is installed, but nothing answers to \`carrick\` on PATH. Add the directory ${command.split(" ")[0]} installs into to PATH, so your agent's hooks can run it by name.`,
    );
    return { kind: "stale", path: null, version: null, command };
  }
  if (after.version === running) {
    options.say(`The global carrick is now ${running}.`);
    return { kind: "upgraded", from: found.version, to: running };
  }
  const theirs = globalCommand(machine.shape(after).kind, running);
  options.say(
    `\`carrick\` still runs ${after.version ?? "an unknown version"} from ${after.binary}. Replace that one with \`${(theirs ?? argv).join(" ")}\`, or take it off PATH.`,
  );
  return { kind: "stale", path: after.binary, version: after.version, command: (theirs ?? argv).join(" ") };
}

/**
 * The commands this never runs on.
 *
 * `hook` and `lsp` speak a protocol and run on every edit; `remove` is somebody
 * undoing this install, and upgrading one on the way out is the opposite of
 * what they asked for. The three argument forms are the ones that answer in
 * milliseconds and are typed to find something out, not to do work.
 */
const NO_SYNC = new Set(["hook", "lsp", "remove", "--version", "-V", "--help", "-h"]);

/**
 * Whether this invocation keeps the global install level.
 *
 * Only a run through npx. Every other shape IS the install a person would be
 * upgrading: a global run of `carrick index` is already the version on PATH,
 * and a project dependency belongs to the repository that pins it. npx is the
 * one that leaves a second, older carrick behind it, and it is also the one the
 * quickstart and the agents use.
 *
 * Never in CI: a build machine is thrown away, nothing on it is on PATH for a
 * next run, and what runs there is the workflow's decision.
 */
export function syncsOnThisRun(
  command: string | undefined,
  env: NodeJS.ProcessEnv = process.env,
  root: string = packageRoot(),
): boolean {
  if (command === undefined || NO_SYNC.has(command)) return false;
  if (suppressed(env) || inCi(env)) return false;
  return installShape(root, env).kind === "npx";
}

export type OfferOutcome = "present" | "installed" | "declined" | "failed";

export type OfferOptions = {
  running?: string | null;
  /** `--yes`, which consents to the scaffold and not to an install. */
  assumeYes: boolean;
  /** `--install-global`, which is the consent. */
  install: boolean;
  confirm: (question: string) => Promise<boolean>;
  say: (line: string) => void;
  machine?: GlobalMachine;
  env?: NodeJS.ProcessEnv;
  root?: string;
};

/**
 * Offer a global install to a machine that has none, and never take one.
 *
 * The ruling: `--yes` answers for the workspace, so it prints the command and
 * installs nothing; a terminal is asked; `--install-global` is the consent a
 * script gives. What turns on the answer is the hook commands written
 * immediately after this — with a global, they are the bare `carrick`, which
 * survives the end of this npx run; without one, they name this install, which
 * does not.
 */
export async function offerGlobalInstall(options: OfferOptions): Promise<OfferOutcome> {
  const env = options.env ?? process.env;
  const machine = options.machine ?? realMachine(env);
  const running = options.running === undefined ? currentVersion() : options.running;
  if (machine.find() !== null) return "present";
  if (!running || !/^\d+\.\d+\.\d+$/.test(running)) return "declined";
  const argv = firstGlobalCommand(installShape(options.root ?? packageRoot(), env).kind, running);
  const command = argv.join(" ");
  const wanted =
    options.install ||
    (!options.assumeYes &&
      (await options.confirm(
        `Install carrick ${running} on this machine with \`${command}\`? Your agent's hooks then run it by name.`,
      )));
  if (!wanted) {
    options.say(
      `\`carrick\` is not on PATH, so the hooks below name this install, which ends with this run. \`${command}\` puts it there for good.`,
    );
    return "declined";
  }
  options.say(`Installing carrick ${running} (${command})`);
  const result = machine.install(argv);
  if (result.ok && machine.find() !== null) return "installed";
  options.say(
    `carrick is still not on PATH: ${result.reason ?? "the install finished but nothing answers to `carrick`"}. Run \`${command}\` yourself for the short hook commands.`,
  );
  return "failed";
}
