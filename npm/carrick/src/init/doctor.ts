// Re-checking, on demand, what the first run decided once.
//
// Everything the index's accuracy depends on is settled at setup time by an
// agent following prose: which directories are services, which env vars name a
// URL, which files the scan reads. The index itself does not decay — CI
// re-scans the default branch on every push — but the configuration does, and
// nothing re-reads it (carrick#1035). `carrick doctor` is that second read.
//
// Three rules hold the whole command together:
//
//  1. **Read-only.** Every check opens files, runs `carrick status`, or asks
//     git a question. None of them writes, and none of them spends money. A
//     command people run when they are already suspicious must not change the
//     thing they are inspecting.
//  2. **A finding is something that is wrong**, not something that is unusual.
//     Exit is non-zero on any finding, so a check that fires on a healthy repo
//     costs the whole command its meaning: the drift check reports lines the
//     template has and yours does not, never lines you added; a local index
//     behind the working tree is a note, because that is what an index looks
//     like while someone is working.
//  3. **Each check lives with what it checks.** The hook and MCP readers are
//     in the same files as the writers whose work they audit, so the audit and
//     the install cannot drift; the workflow is compared against the template
//     this package renders, not against a copy.
//
// What is NOT here, and where it is: the scaffold's hook-pack scripts
// (`.claude/*.sh`) are the cloud's bytes and are compared by its own
// `hook_pack` drift tool, not offline; scan coverage and env-var declarations
// need the scanner's file walk and its extraction pass, and are carrick#1053.

import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

import { status as runStatus } from "../cli.ts";
import { nativeEnv, resolveNativeBinary } from "../native.ts";
import { DEFAULTS, renderTemplate, TEMPLATE_PATHS } from "../templates.ts";
import { inspectMcpClients, mcpLine, type McpInspection } from "./mcp.ts";
import { readInstallId } from "./install-id.ts";
import { createOutput, DOCS, type InitOutput } from "./output.ts";
import { repoRoots } from "./repos.ts";
import {
  expectedCarrickHooks,
  hookTarget,
  installedCarrickHooks,
  ownEntryPoint,
  type InstalledHook,
} from "./settings.ts";
import { SETTINGS_FILES } from "./remove.ts";
import type { StatusResult } from "../contract.ts";

export type DoctorOptions = { workspace: string };

export function parseArgs(argv: string[], cwd = process.cwd()): DoctorOptions | string {
  const options: DoctorOptions = { workspace: cwd };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    switch (argument) {
      case "--workspace":
      case "-w": {
        const value = argv[index + 1];
        if (!value) return "--workspace needs a directory";
        options.workspace = path.resolve(cwd, value);
        index += 1;
        break;
      }
      case "--help":
      case "-h":
        return help();
      default:
        if (argument?.startsWith("-")) return `unknown option for \`carrick doctor\`: ${argument}`;
        options.workspace = path.resolve(cwd, argument ?? ".");
    }
  }
  return options;
}

function help(): string {
  return [
    "carrick doctor [DIRECTORY]",
    "",
    "Re-check the setup a first run decided once: the paths every carrick.json",
    "declares, the CI workflow against the current template, the agent hooks and",
    "the MCP connection on this machine, and how far the index is behind. It",
    "writes nothing, runs no scan, and exits non-zero when it finds something.",
    "",
    "    -w, --workspace DIR  The folder init was run in (default: this one)",
    "",
    `What each of those things is, and what it does: ${DOCS}`,
  ].join("\n");
}

/** One line of the report. `warn` and `refuse` are findings; the rest are not. */
export type Line = { level: "done" | "warn" | "refuse" | "say"; text: string };

export function findingCount(lines: Line[]): number {
  return lines.filter((line) => line.level === "warn" || line.level === "refuse").length;
}

const done = (text: string): Line => ({ level: "done", text });
const warn = (text: string): Line => ({ level: "warn", text });
const refuse = (text: string): Line => ({ level: "refuse", text });
const say = (text: string): Line => ({ level: "say", text });

/** A repo of the workspace, with the configuration it declares. */
export type ConfiguredRepo = {
  /** Absolute. */
  root: string;
  /** Workspace-relative, or the basename when the workspace IS the repo. */
  label: string;
  /** Parsed `carrick.json`, null when there is none. */
  config: Record<string, unknown> | null;
  /** Why there is no config, when the file exists and could not be read. */
  problem: string | null;
};

export function configuredRepos(workspace: string): ConfiguredRepo[] {
  return repoRoots(workspace).map((root) => {
    const label = path.relative(workspace, root) || path.basename(root);
    const file = path.join(root, "carrick.json");
    if (!fs.existsSync(file)) return { root, label, config: null, problem: null };
    try {
      const parsed: unknown = JSON.parse(fs.readFileSync(file, "utf8"));
      if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
        return { root, label, config: null, problem: "carrick.json is not a JSON object" };
      }
      return { root, label, config: parsed as Record<string, unknown>, problem: null };
    } catch (error) {
      return { root, label, config: null, problem: `carrick.json is not valid JSON: ${(error as Error).message}` };
    }
  });
}

/** One service entry of a `carrick.json`, as this command reads it. */
type DeclaredService = {
  name: string;
  directory?: string;
  tsconfig?: string;
  include: string[];
};

/**
 * The services a config declares: the `services` array, or the file itself.
 *
 * The same resolution as `Config::load_services` in src/config.rs, for the
 * three fields that name a path. When `services` is present its sibling flat
 * fields are ignored, which is the rule the scanner applies too.
 */
export function declaredServices(config: Record<string, unknown>): DeclaredService[] {
  const entries = Array.isArray(config["services"]) && config["services"].length > 0
    ? (config["services"] as unknown[])
    : [config];
  const services: DeclaredService[] = [];
  for (const [index, raw] of entries.entries()) {
    if (typeof raw !== "object" || raw === null) continue;
    const entry = raw as Record<string, unknown>;
    const named = entry["serviceName"] ?? entry["name"];
    const directory = typeof entry["directory"] === "string" ? entry["directory"] : undefined;
    const service: DeclaredService = {
      name: typeof named === "string" && named !== "" ? named : (directory ?? `service ${index + 1}`),
      include: Array.isArray(entry["include"])
        ? (entry["include"] as unknown[]).filter((value): value is string => typeof value === "string")
        : [],
    };
    if (directory !== undefined) service.directory = directory;
    if (typeof entry["tsconfig"] === "string") service.tsconfig = entry["tsconfig"];
    services.push(service);
  }
  return services;
}

/**
 * Every path a `carrick.json` declares, and whether it is there.
 *
 * The bases are the scanner's, read off the code that joins them rather than
 * off the field names: `directory` and every `include` are relative to the
 * repository root, which is where `carrick.json` sits (`find_service_files`),
 * and `tsconfig` is relative to the SERVICE directory, because that is the
 * root the type sidecar is initialised at (`scope_sidecar_to_service`). A
 * doctor that resolved `tsconfig` from the repo root would report a missing
 * file on every monorepo that has one.
 */
export function checkDeclaredPaths(repos: ConfiguredRepo[]): Line[] {
  const lines: Line[] = [];
  // A file that exists and will not parse is not an absent one: the two states
  // have different sentences, and printing both would be printing a false one.
  let files = 0;
  let configs = 0;
  let checked = 0;
  for (const repo of repos) {
    if (repo.problem !== null) {
      files += 1;
      lines.push(refuse(`${repo.label}: ${repo.problem}`));
      continue;
    }
    if (repo.config === null) {
      lines.push(say(`${repo.label} has no carrick.json, so nothing in it is indexed.`));
      continue;
    }
    files += 1;
    configs += 1;
    for (const service of declaredServices(repo.config)) {
      const serviceRoot = service.directory === undefined
        ? repo.root
        : path.resolve(repo.root, service.directory);
      if (service.directory !== undefined) {
        checked += 1;
        if (!isDirectory(serviceRoot)) {
          lines.push(
            refuse(
              `${repo.label}: service "${service.name}" declares directory "${service.directory}", which is not a directory in this repo. Nothing under it is scanned.`,
            ),
          );
        }
      }
      for (const include of service.include) {
        checked += 1;
        if (!isDirectory(path.resolve(repo.root, include))) {
          lines.push(
            refuse(
              `${repo.label}: service "${service.name}" includes "${include}", which is not a directory in this repo.`,
            ),
          );
        }
      }
      if (service.tsconfig !== undefined) {
        checked += 1;
        const tsconfig = path.resolve(serviceRoot, service.tsconfig);
        if (!isFile(tsconfig)) {
          lines.push(
            refuse(
              `${repo.label}: service "${service.name}" declares tsconfig "${service.tsconfig}", which is not a file at ${path.relative(repo.root, tsconfig) || service.tsconfig}. Its types resolve from whatever the sidecar finds instead.`,
            ),
          );
        }
      }
    }
  }
  if (files === 0) {
    lines.push(
      refuse(
        "No carrick.json anywhere in this workspace, so no service is declared. `carrick init` derives one for your agent to write.",
      ),
    );
    return lines;
  }
  if (findingCount(lines) === 0) {
    lines.push(done(`Declared paths exist: ${checked} in ${configs} carrick.json file(s).`));
  }
  return lines;
}

function isDirectory(target: string): boolean {
  try {
    return fs.statSync(target).isDirectory();
  } catch {
    return false;
  }
}

function isFile(target: string): boolean {
  try {
    return fs.statSync(target).isFile();
  } catch {
    return false;
  }
}

/** The lines of a file that state behaviour: no comments, no blank lines. */
export function functionalLines(body: string): string[] {
  return body
    .replace(/\r\n/g, "\n")
    .split("\n")
    .map((line) => line.trimEnd())
    .filter((line) => line.trim() !== "" && !line.trim().startsWith("#"));
}

export type Drift = {
  /** Template lines this file does not have. The finding. */
  missing: string[];
  /** Lines this file has that the template does not. Context, never a finding. */
  added: string[];
  /** Both, interleaved in file order, as `-`/`+` lines. */
  diff: string[];
};

/**
 * What one file differs from a rendered template by, comment-insensitively.
 *
 * Comments and blank lines are dropped from both sides first. The template is
 * two fifths comment, so comparing them would report a finding for every line
 * a reader trimmed and for every rewording of an explanation — neither of
 * which changes what CI runs.
 *
 * Only `missing` is a finding. A workflow with steps of its own is the case
 * the template itself invites ("If you add deploy steps to this file..."), so
 * added lines are printed as context and fail nothing.
 */
export function templateDrift(actual: string, expected: string): Drift {
  const have = functionalLines(actual);
  const want = functionalLines(expected);
  // Longest common subsequence over lines: small inputs, and the alignment is
  // what makes the printed diff readable rather than "these two files differ".
  const table: number[][] = Array.from({ length: want.length + 1 }, () =>
    new Array<number>(have.length + 1).fill(0),
  );
  for (let w = want.length - 1; w >= 0; w -= 1) {
    for (let h = have.length - 1; h >= 0; h -= 1) {
      table[w]![h] = want[w] === have[h]
        ? table[w + 1]![h + 1]! + 1
        : Math.max(table[w + 1]![h]!, table[w]![h + 1]!);
    }
  }
  const missing: string[] = [];
  const added: string[] = [];
  const diff: string[] = [];
  let w = 0;
  let h = 0;
  while (w < want.length && h < have.length) {
    if (want[w] === have[h]) {
      w += 1;
      h += 1;
    } else if (table[w + 1]![h]! >= table[w]![h + 1]!) {
      missing.push(want[w]!);
      diff.push(`- ${want[w]}`);
      w += 1;
    } else {
      added.push(have[h]!);
      diff.push(`+ ${have[h]}`);
      h += 1;
    }
  }
  for (; w < want.length; w += 1) {
    missing.push(want[w]!);
    diff.push(`- ${want[w]}`);
  }
  for (; h < have.length; h += 1) {
    added.push(have[h]!);
    diff.push(`+ ${have[h]}`);
  }
  return { missing, added, diff };
}

const WORKFLOW_PATH = TEMPLATE_PATHS.workflow;

/**
 * The variables a workflow on disk states, so the comparison is about drift
 * rather than about the two values the template exists to parameterise.
 *
 * A repo pinned to an older action ref, or building on a branch that is not
 * `main`, is not drift: it is the template with its variables filled in. Both
 * are read back out of the file, and where one cannot be read the default is
 * used and the caller says so.
 */
export function workflowVariables(body: string): { variables: Record<string, string>; unread: string[] } {
  const variables: Record<string, string> = {};
  const unread: string[] = [];
  const uses = /^\s*-?\s*uses:\s*(\S*carrick-tools\/carrick@\S+)\s*$/m.exec(body);
  if (uses) variables["ACTION_REF"] = uses[1]!;
  else unread.push("ACTION_REF");
  const branches = new Set<string>();
  for (const match of body.matchAll(/^\s*branches:\s*\[([^\]]*)\]\s*$/gm)) {
    for (const name of match[1]!.split(",")) {
      const trimmed = name.trim().replace(/^["']|["']$/g, "");
      if (trimmed !== "") branches.add(trimmed);
    }
  }
  if (branches.size === 1) variables["DEFAULT_BRANCH"] = [...branches][0]!;
  else unread.push("DEFAULT_BRANCH");
  return { variables, unread };
}

/**
 * The CI workflow of every configured repo, against the template this package
 * renders.
 *
 * Only repos with a `carrick.json` are asked for one: a sibling clone that
 * Carrick does not index owes CI nothing.
 */
export function checkWorkflow(repos: ConfiguredRepo[]): Line[] {
  const lines: Line[] = [];
  let matched = 0;
  for (const repo of repos) {
    if (repo.config === null) continue;
    const file = path.join(repo.root, WORKFLOW_PATH);
    let body: string;
    try {
      body = fs.readFileSync(file, "utf8");
    } catch {
      lines.push(
        warn(
          `${repo.label}: no ${WORKFLOW_PATH}, so pushes to the default branch do not re-index it. Write one with \`carrick templates workflow\`.`,
        ),
      );
      continue;
    }
    const { variables, unread } = workflowVariables(body);
    const drift = templateDrift(body, renderTemplate("workflow", variables));
    if (drift.missing.length === 0) {
      matched += 1;
      if (drift.added.length > 0) {
        lines.push(
          say(
            `${repo.label}: ${WORKFLOW_PATH} has the current template in it, plus ${drift.added.length} line(s) of your own.`,
          ),
        );
      }
      continue;
    }
    const caveat = unread.includes("ACTION_REF")
      ? " No step in it runs the Carrick action."
      : unread.includes("DEFAULT_BRANCH")
        ? ` Its branch list could not be read, so it was compared against ${DEFAULTS["DEFAULT_BRANCH"]}.`
        : "";
    lines.push(
      warn(
        `${repo.label}: ${WORKFLOW_PATH} is missing ${drift.missing.length} line(s) of the current template.${caveat} Comments are not compared; \`-\` is the template's, \`+\` is yours:\n${drift.diff.map((line) => `    ${line}`).join("\n")}`,
      ),
    );
  }
  if (matched > 0 && findingCount(lines) === 0) {
    lines.push(done(`CI workflow matches the current template in ${matched} repo(s).`));
  }
  return lines;
}

/** What the hook check needs to know about this machine. */
export type HookMachine = {
  /** The absolute path `command` resolves to on PATH, through symlinks. */
  resolveOnPath: (command: string) => string | null;
  /** The real path of a file, or null when it is not on this machine. */
  realpath: (target: string) => string | null;
  /** This package's own entry point, the one a settings file would name. */
  entryPoint: string;
};

export function realMachine(): HookMachine {
  return {
    resolveOnPath: (command) => {
      const probe = spawnSync(process.platform === "win32" ? "where" : "which", [command], {
        encoding: "utf8",
        timeout: 5000,
      });
      if (probe.status !== 0 || typeof probe.stdout !== "string") return null;
      const first = probe.stdout.split("\n")[0]?.trim();
      if (!first) return null;
      try {
        return fs.realpathSync(first);
      } catch {
        return first;
      }
    },
    realpath: (target) => {
      try {
        return fs.realpathSync(target);
      } catch {
        return null;
      }
    },
    entryPoint: ownEntryPoint(),
  };
}

/**
 * The hook entries in this workspace's `.claude` settings.
 *
 * Three separate questions, and a hook fails silently on all three, because
 * the hook is built never to fail an edit (carrick#837): are the entries
 * there, are they the ones this version writes, and does the command they name
 * still run on this machine?
 *
 * The last one is where an install rots. An entry written as a bare `carrick`
 * needs `carrick` on PATH at every edit; an entry written under `npx` resolves
 * for the length of that one command and never again; an entry naming an
 * absolute path is stale the moment that install is replaced.
 */
export function checkHooks(workspace: string, machine: HookMachine): Line[] {
  const lines: Line[] = [];
  const installed: InstalledHook[] = [];
  let files = 0;
  for (const relative of SETTINGS_FILES) {
    const file = path.join(workspace, relative);
    let body: string;
    try {
      body = fs.readFileSync(file, "utf8");
    } catch {
      continue;
    }
    files += 1;
    try {
      installed.push(...installedCarrickHooks(body));
    } catch (error) {
      lines.push(refuse(`${relative} is not valid JSON (${(error as Error).message}), so no hook in it runs.`));
    }
  }
  if (installed.length === 0) {
    if (findingCount(lines) > 0) return lines;
    lines.push(
      warn(
        files === 0
          ? `No .claude settings in this folder, so no Carrick hook runs here. \`carrick init\` writes them.`
          : `No Carrick hook entries in ${SETTINGS_FILES.join(" or ")}, so nothing re-checks a file when your agent edits it. \`carrick init\` writes them.`,
      ),
    );
    return lines;
  }

  // Every entry names the same command, so the first one states which install
  // the settings file points at and the rest are compared against it. The
  // prefix is taken as written, quotes and all, because that is what the
  // writer put there and what the expected entries are rendered from.
  const prefix = installed[0]!.command.split(/\s+hook\s+/)[0]!;
  const target = hookTarget(installed[0]!.command);
  const expected = expectedCarrickHooks(prefix);
  const missing = expected.filter(
    (want) =>
      !installed.some(
        (have) =>
          have.event === want.event &&
          have.command === want.command &&
          have.matcher === want.matcher &&
          have.timeout === want.timeout,
      ),
  );
  if (missing.length > 0) {
    lines.push(
      warn(
        `The hook entries here are not the ones this version installs: ${missing
          .map((entry) => `${entry.event} \`${entry.command}\`${entry.matcher ? ` (matcher ${entry.matcher})` : ""}`)
          .join(", ")} ${missing.length === 1 ? "is" : "are"} not in ${SETTINGS_FILES.join(" or ")}. Re-run \`carrick init\`.`,
      ),
    );
  }

  if (target === null) {
    lines.push(refuse(`A hook entry here runs \`${installed[0]!.command}\`, which names no command.`));
    return lines;
  }
  const resolved = path.isAbsolute(target) ? machine.realpath(target) : machine.resolveOnPath(target);
  if (resolved === null) {
    lines.push(
      refuse(
        path.isAbsolute(target)
          ? `The hooks here run ${target}, which is not a file on this machine, so every edit fails silently. Re-run \`carrick init\`.`
          : `The hooks here run \`${target}\`, which does not resolve on PATH, so every edit fails silently. Install it globally (\`npm install -g carrick\`) or re-run \`carrick init\`, which writes an absolute path when it has to.`,
      ),
    );
    return lines;
  }
  if (isTransient(resolved)) {
    lines.push(
      warn(
        `The hooks here run \`${target}\`, which resolves to a temporary npx install (${resolved}). It will not resolve on the next edit. Install carrick globally and re-run \`carrick init\`.`,
      ),
    );
    return lines;
  }

  // Which install answers, and whether that question can be answered at all.
  //
  // Comparing the resolved path to this package's entry point only means
  // something when what resolved IS an entry point. A pnpm global install and
  // every Windows install put a shim on PATH — a shell script, a `.cmd` — and
  // a shim does not realpath to `bin/carrick.mjs`, so a comparison would
  // report two installs on a machine that has one. And when `carrick doctor`
  // is itself run through `npx`, the transient copy is the one asking: the
  // hooks are pointing at the real install and the sentence would blame them
  // for it. In both cases the check that matters has already passed — the
  // command resolves, so the hook will not fail silently — and identity is
  // reported as what it is: unproven.
  const own = machine.realpath(machine.entryPoint) ?? machine.entryPoint;
  const entryPointName = path.basename(machine.entryPoint);
  if (path.basename(resolved) !== entryPointName) {
    if (findingCount(lines) === 0) {
      lines.push(done(`Agent hooks are installed here and run \`${target}\`, which resolves to ${resolved}.`));
    }
    return lines;
  }
  if (isTransient(own)) {
    if (findingCount(lines) === 0) {
      lines.push(
        done(
          `Agent hooks are installed here and run ${resolved}. This check is running from a temporary npx install, so it cannot say whether that is the same one.`,
        ),
      );
    }
    return lines;
  }
  if (resolved !== own) {
    lines.push(
      warn(
        `The hooks here run ${resolved}; this is ${own}. Two installs answer for one machine, and the hooks use the other one.`,
      ),
    );
    return lines;
  }
  if (findingCount(lines) === 0) {
    lines.push(done(`Agent hooks are installed here and run this package (${own}).`));
  }
  return lines;
}

/** A path inside npm's `_npx` cache: it exists for one command and no longer. */
function isTransient(target: string): boolean {
  return target.includes(`${path.sep}_npx${path.sep}`);
}

/** The MCP server entry in each agent client this machine has. */
export function checkMcp(inspections: McpInspection[]): Line[] {
  if (inspections.length === 0) {
    return [say("No agent client on this machine holds an MCP configuration, so there is none to check.")];
  }
  const lines: Line[] = [];
  const connected: string[] = [];
  // Read, never created: this command writes nothing, so a machine that has
  // never run `carrick init` is told the line without an id rather than given
  // one as a side effect of being inspected.
  const byHand = mcpLine(readInstallId());
  for (const client of inspections) {
    if (client.state === "connected") {
      connected.push(client.client);
      continue;
    }
    if (client.state === "absent") {
      lines.push(
        warn(
          `${client.client} is on this machine and is not connected to Carrick (${client.detail}), so it answers nothing across your repos. \`carrick init\` connects it; by hand it is \`${byHand}\`.`,
        ),
      );
      continue;
    }
    lines.push(warn(`${client.client}: ${client.detail}.`));
  }
  if (connected.length > 0 && findingCount(lines) === 0) {
    lines.push(done(`MCP server connected for ${connected.join(", ")}.`));
  }
  return lines;
}

/** Read-only git, for how far a commit is behind the branch CI indexes. */
export type GitReader = {
  /** `origin/<default>`, or null when this clone does not say. */
  defaultRemoteBranch: (repo: string) => string | null;
  /** Commits in `<from>..<to>`, or null when either end is unknown here. */
  commitsBetween: (repo: string, from: string, to: string) => number | null;
};

export function realGit(): GitReader {
  const git = (repo: string, args: string[]): string | null => {
    const result = spawnSync("git", ["-C", repo, ...args], { encoding: "utf8", timeout: 5000 });
    if (result.error || result.status !== 0 || typeof result.stdout !== "string") return null;
    return result.stdout.trim();
  };
  return {
    defaultRemoteBranch: (repo) => git(repo, ["rev-parse", "--abbrev-ref", "origin/HEAD"]),
    commitsBetween: (repo, from, to) => {
      const count = git(repo, ["rev-list", "--count", `${from}..${to}`]);
      if (count === null) return null;
      const parsed = Number.parseInt(count, 10);
      return Number.isFinite(parsed) ? parsed : null;
    },
  };
}

/**
 * The states a hosted index can be in that a reader can do something about.
 *
 * `enriched` is the healthy one. Every other state means this machine is
 * answering from less than the index holds, and the CLI's own sentence for it
 * (`boundary_note`) already names the next move, so it is printed rather than
 * re-worded here.
 */
const HEALTHY_HOSTED_STATE = "enriched";

/**
 * How far the index is behind, and whether the hosted half arrived at all.
 *
 * Two different questions, and only one of them is a finding. A hosted state
 * that is not `enriched` is plumbing: not connected, not signed in, no index
 * yet, a cache version this build cannot replay. Files changed since the index
 * are not: that is what a repository looks like while someone is working in
 * it, and CI re-indexes the default branch on every push. So the first is a
 * finding and the second is a note with the numbers in it.
 */
export function checkIndex(
  result: StatusResult | null,
  failure: string | null,
  git: GitReader,
): Line[] {
  if (result === null) {
    return [refuse(`Could not read the local index: ${failure ?? "the scanner gave no answer"}.`)];
  }
  if (result.error) {
    return [refuse(result.message ?? `The local index could not be read: ${result.error}.`)];
  }
  const lines: Line[] = [];
  for (const service of result.services) {
    const state = service.hosted_state;
    if (state === undefined || state === HEALTHY_HOSTED_STATE) continue;
    // The CLI's own sentence leads, because it is the one that names the next
    // move; the state itself is the word to quote in a bug report, so it goes
    // at the end rather than in front of the explanation.
    const sentence =
      service.boundary_note ??
      "This machine is answering from less than the index holds. `carrick init` connects a repo, `carrick login` signs this machine in.";
    lines.push(warn(`${service.service}: ${sentence} (hosted state: ${state})`));
  }
  if (findingCount(lines) === 0 && result.services.length > 0) {
    lines.push(done(`The hosted index answers for all ${result.services.length} indexed service(s).`));
  }

  for (const repo of result.repos ?? []) {
    if (repo.changed_since_index > 0) {
      lines.push(
        say(
          `${repo.name}: ${repo.changed_since_index} file(s) have changed since this index was built, and are answered from the tree, not the index.`,
        ),
      );
    }
    // The path the CLI reported, not one this command resolved: two spellings
    // of one directory are two directories to git as much as to anything else.
    const commit = result.services.find((entry) => entry.repo === repo.repo)?.index_commit;
    if (!commit) continue;
    const branch = git.defaultRemoteBranch(repo.repo);
    if (branch === null) continue;
    const behind = git.commitsBetween(repo.repo, commit, branch);
    if (behind === null) {
      lines.push(say(`${repo.name}: indexed at ${short(commit)}, a commit this clone does not hold.`));
    } else if (behind > 0) {
      lines.push(
        say(`${repo.name}: indexed at ${short(commit)}, ${behind} commit(s) behind ${branch}.`),
      );
    }
  }
  return lines;
}

function short(commit: string): string {
  return commit.slice(0, 7);
}

/** Print one check's lines through the shared renderer. */
function print(out: InitOutput, lines: Line[]): void {
  for (const line of lines) {
    if (line.level === "done") out.done(line.text);
    else if (line.level === "warn") out.warn(line.text);
    else if (line.level === "refuse") out.refuse(line.text);
    else out.say(line.text);
  }
}

export async function doctor(argv: string[], out: InitOutput = createOutput()): Promise<number> {
  const parsed = parseArgs(argv);
  if (typeof parsed === "string") {
    process.stdout.write(`${parsed}\n`);
    return parsed.startsWith("carrick doctor") ? 0 : 2;
  }
  const { workspace } = parsed;
  if (!fs.existsSync(workspace)) {
    process.stderr.write(`carrick doctor: ${workspace} is not a directory on this machine\n`);
    return 1;
  }

  const repos = configuredRepos(workspace);
  const lines: Line[] = [
    ...checkDeclaredPaths(repos),
    ...checkWorkflow(repos),
    ...checkHooks(workspace, realMachine()),
    ...checkMcp(inspectMcpClients()),
    ...(await readIndex(workspace)),
  ];
  print(out, lines);

  const findings = findingCount(lines);
  if (findings === 0) {
    out.say("");
    out.say("Nothing to fix.");
    return 0;
  }
  out.say("");
  out.say(
    `${findings} finding(s) above, marked ▲ or ■. Nothing here was changed; ` +
      `\`carrick doctor\` exits non-zero while any of them stand.`,
  );
  return 1;
}

/**
 * `carrick status --json`, with a limit long enough for a cold read.
 *
 * The shared runner's default is five seconds, which is the editor's budget,
 * not a person's: a status killed by it prints nothing at all and the check
 * above would report a healthy index as unreadable (carrick#1036).
 */
async function readIndex(workspace: string): Promise<Line[]> {
  const lookup = resolveNativeBinary();
  if (!lookup.binary) {
    return [refuse(lookup.problem ?? "The Carrick scanner is not installed, so the index cannot be read.")];
  }
  const outcome = await runStatus({
    cwd: workspace,
    workspace,
    bin: lookup.binary,
    env: { ...nativeEnv(), CARRICK_TIMEOUT_MS: "30000" },
  });
  return checkIndex(outcome.result, outcome.failure, realGit());
}
