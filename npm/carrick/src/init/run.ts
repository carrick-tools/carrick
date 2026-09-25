// Authenticated setup; Rust owns the workspace and service proposal.
//
// What this prints is one line per thing that happened (carrick#1026). The
// reasoning behind each step — how the hooks deliver, which editor extension
// to install, why the scan is left to CI, what a hand-written carrick.json
// looks like — is in the docs, at the `DOCS*` links in `output.ts`, and this
// command links to them rather than repeating them. The record of the run stays here,
// because nothing else can state it: this login, this project, these repos,
// this many packages, this index.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { readCredential, type Credential } from "../auth/credentials.ts";
import { openBrowser } from "../auth/oauth.ts";
import { signIn } from "../auth/run.ts";
import { resolveRepos, type ResolvedRepos } from "../auth/read.ts";
import {
  deriveWorkspace,
  selectedProposal,
  selectRepos,
  siblingRepos,
  writeProposal,
  realPath,
  repoRoots,
  MAX_SIBLINGS,
  PROPOSAL_FILE,
  repoIdentity,
  type DerivedWorkspace,
  type RepoIdentity,
  type WorkspaceProposal,
} from "./repos.ts";
import {
  connectLine,
  connectRepos,
  projectAssignments,
  reposAreInProject,
  unconnectedRepos,
  workspaceUrls,
} from "./connect.ts";
import { downloadHostedIndex, hostedReport, nativeRunner, STEP_LABEL } from "./hosted.ts";
import {
  createProject,
  listProjects,
  planProject,
  projectLabel,
  projectStep,
  reposPhrase,
  SLUG,
  type Project,
  type ProjectChoice,
  type ProjectPrompts,
} from "./projects.ts";
import { writeRepoCopy } from "./repo-copies.ts";
import { claudeCodeFound, connectMcpClients, mcpLine, offeredFileClients, type OfferedClient } from "./mcp.ts";
import { hookCommand, mergeCarrickHooks, removeCarrickHooks } from "./settings.ts";
import { CODEX_HOOKS_FILE, writeCodexHooks } from "./codex.ts";
import { ignoredSkillRoots, taskSkillLines, writeTaskSkills } from "./task-skills.ts";
import { recordCliVersion } from "./outdated.ts";
import { findCarrick, offerGlobalInstall } from "../global-install.ts";
import { currentVersion } from "../update.ts";
import { writeIfChanged } from "./files.ts";
import { excludedRepos, writeSelection, WORKSPACE_FILE } from "./workspace-file.ts";
import { createOutput, DOCS_EDITOR, DOCS_INDEX, DOCS_INIT_FILES, PromptCancelled, type Choice, type InitOutput } from "./output.ts";
import { renderTemplate, TEMPLATE_PATHS } from "../templates.ts";

/**
 * The repos this run covers that the scaffold tool still has work in.
 *
 * The two files that tool writes: the `carrick.json` a scan reads, and the
 * workflow that runs the scan. A repo holding both has been scaffolded, and
 * the closing instruction is then an instruction to do nothing — which is what
 * it was on a folder where every repo was already set up (carrick#1365).
 *
 * The repo root only, because that is where the scanner looks for a config
 * (`Config::load_services`): a `carrick.json` deeper in the tree is a fixture
 * or a nested example, not this repo's.
 */
export function reposToScaffold(
  repos: RepoIdentity[],
  exists: (target: string) => boolean = fs.existsSync,
): RepoIdentity[] {
  return repos.filter(
    (repo) =>
      !exists(path.join(repo.path, TEMPLATE_PATHS["carrick.json"])) ||
      !exists(path.join(repo.path, TEMPLATE_PATHS.workflow)),
  );
}

/**
 * The instruction to paste, naming the repos it is about.
 *
 * The scaffold tool's branch turns on `repo` and turns soft without it: with
 * no `repo` the response keeps its default ending and tells the agent to run
 * the scan — on a repo CI already indexes, which is the row the whole
 * workspace reads (carrick-cloud `src/tools/scaffold.ts`, cloud#805 item 1).
 * So the names are in the sentence, one call per repo.
 */
export function scaffoldSentence(repos: RepoIdentity[]): string {
  const names = repos.map((repo) => repo.name ?? path.basename(repo.path));
  if (names.length === 1) {
    return `Run the carrick scaffold tool for ${names[0]}, passing its owner/repo as \`repo\`, and follow what it returns.`;
  }
  const shown = names.length > 3 ? `${names.slice(0, 3).join(", ")} and ${names.length - 3} more` : names.join(", ");
  return `Run the carrick scaffold tool for ${shown}, once each, passing that repo's owner/repo as \`repo\`, and follow what it returns.`;
}

/** The title of the block a run closes on when there is scaffolding left to do. */
export const NEXT_TITLE = "Next: paste this into a new agent session";

/**
 * The line that tells the agent where the proposal is, for a run that set up
 * the folder above the one it started in (carrick#1512).
 */
export function initFolderSentence(workspace: string): string {
  return `The init folder is ${tilde(workspace)}.`;
}

/** What a run with nothing left to set up closes on: what this machine can do now. */
export const READY_SENTENCE =
  "The index is on this machine: `carrick check <file>` answers for a file, and your agent can ask who calls an operation.";

export type InitOptions = {
  workspace: string;
  /** Put the selected repos in this project, moving them where `allowMove` says. */
  project: string | null;
  /**
   * The repos this install covers, `owner/repo` each, repeatable and
   * comma-separable. Empty means the terminal picks them (carrick#1338).
   *
   * It is also how a repository whose remote names none is named
   * (carrick#991): a value that matches nothing on disk attaches to the one
   * repo here that has no identity.
   */
  repos: string[];
  /**
   * The editors outside this workspace to add the MCP entry to, by name.
   *
   * Empty means the terminal asks, and a run with no terminal writes no editor
   * file at all: `--yes` is an answer about this workspace (carrick#1365).
   */
  editors: string[];
  /** Answer yes to the proposal rather than asking. Never grants a move. */
  assumeYes: boolean;
  /** Take the selected repos out of whatever project they are in now. */
  allowMove: boolean;
  /**
   * Install carrick globally where this machine has none (carrick#1372).
   *
   * Its own flag, because `--yes` is an answer about this workspace and a
   * global install is a change to the machine. Without a global, the hooks
   * written below name this install — an npx cache directory, when that is how
   * this ran — and stop working when it is cleared.
   */
  installGlobal: boolean;
};

const OWNER_REPO = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;

export function parseArgs(argv: string[], cwd = process.cwd()): InitOptions | string {
  const options: InitOptions = {
    workspace: cwd,
    project: null,
    repos: [],
    editors: [],
    assumeYes: false,
    allowMove: false,
    installGlobal: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    switch (argument) {
      case "--yes":
      case "-y":
        options.assumeYes = true;
        break;
      case "--install-global":
        options.installGlobal = true;
        break;
      case "--allow-move":
        options.allowMove = true;
        break;
      case "--project": {
        const value = argv[index + 1];
        if (!value) return "--project needs a slug";
        if (!SLUG.test(value)) {
          return `invalid project slug "${value}": use 3-32 lowercase letters, digits, and single hyphens`;
        }
        options.project = value;
        index += 1;
        break;
      }
      case "--mcp": {
        const value = argv[index + 1];
        if (!value) return "--mcp needs an editor name";
        for (const name of value.split(",")) {
          const trimmed = name.trim();
          if (trimmed === "") return `invalid editor "${name}": name it as carrick init prints it`;
          if (!options.editors.includes(trimmed)) options.editors.push(trimmed);
        }
        index += 1;
        break;
      }
      case "--repo": {
        const value = argv[index + 1];
        if (!value) return "--repo needs an owner/repo";
        for (const name of value.split(",")) {
          if (!OWNER_REPO.test(name)) return `invalid repo "${name}": use owner/repo as GitHub spells it`;
          if (!options.repos.includes(name)) options.repos.push(name);
        }
        index += 1;
        break;
      }
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
        if (argument?.startsWith("-")) return `unknown option for \`carrick init\`: ${argument}`;
        options.workspace = path.resolve(cwd, argument ?? ".");
    }
  }
  return options;
}

function help(): string {
  return [
    "carrick init [DIRECTORY] [--repo OWNER/REPO]... [--project SLUG] [--mcp EDITOR] [--allow-move]",
    "",
    "Sign in (here, or beforehand with carrick login), then set up a repository",
    "or a folder of repos: which repos this install covers, the project, the repo",
    "connection, the agent hooks, the MCP connection, and the service proposal",
    "your agent turns into carrick.json. It writes nothing into the repository but",
    "the ignored .carrick directory, the hook settings, and the four task skills",
    "your agent loads. No model runs and nothing is uploaded. Where Carrick already",
    "holds an index for these repos, it reads that index into .carrick so this",
    "machine can answer from it. That read re-reads your source here first, which",
    "takes minutes on a large workspace. It skips the re-read when .carrick already",
    "holds an index current for this checkout. Nothing is written, here or in",
    "Carrick, until you accept what it proposes.",
    "",
    "    -w, --workspace DIR  The folder holding the repos (default: this one)",
    "        --repo OWNER/REPO  A repo this install covers: repeatable, or one",
    "                         comma-separated list. In a folder of repos without a",
    "                         terminal to choose in, this is required. It also names",
    "                         the GitHub repo whose origin remote names none",
    "        --project SLUG   Put those repos in this Carrick project, creating it",
    "                         if it is not there. A repo that is in another project",
    "                         is MOVED out of it, which changes what every agent",
    "                         querying either project can see, so the move is named",
    "                         and asked about separately",
    "        --allow-move     Accept those moves without being asked. --yes does not",
    "        --mcp EDITOR     Also add Carrick to this editor's own MCP configuration,",
    "                         which is a file outside this workspace: repeatable, or one",
    "                         comma-separated list. Without a terminal no editor file is",
    "                         written unless this names one, and --yes does not name one",
    "    -y, --yes            Take the proposal as printed",
    "        --install-global Install carrick on this machine where there is none,",
    "                         so the agent hooks can run it by name after this run",
    "                         ends. A terminal is asked instead; --yes is not this",
    "",
    `What init installs, the hooks included: ${DOCS_INIT_FILES}`,
    `CI and a carrick.json written by hand: ${DOCS_INDEX}`,
    `The editor extension: ${DOCS_EDITOR}`,
  ].join("\n");
}

/**
 * Whether `carrick` answers on this machine after this run ends.
 *
 * Not `which`: npm puts the exec tree's own `node_modules/.bin` first on PATH
 * for the child it runs, so inside `npx carrick init` a `which carrick` says
 * yes on a machine with no global — and the hooks were then written as a bare
 * `carrick hook post-edit`, naming a command that stopped existing when npx
 * exited (carrick#1372). `findCarrick` skips the copy doing the looking.
 */
function carrickOnPath(): boolean {
  return findCarrick() !== null;
}

/**
 * The repos a project holds that this machine does not, one line per project.
 *
 * Carrick answers across every repo in a project, so a machine holding half of
 * one gets half the answers, and the connection line above names only what is
 * here. `project_repos` is the workspace read's answer to that: the whole
 * membership of every project the requested repos are in (carrick#993 row 18).
 * Capped and counted like the identity lines, because a project can hold two
 * hundred.
 *
 * `onMachine` is every GitHub repo found on this machine, not only the ones
 * this run set up: a repo in the folder beside this one is on the machine, and
 * saying otherwise was the false line in carrick#1512.
 */
export function absentRepos(
  identity: ResolvedRepos,
  onMachine: string[],
  project: string | null,
  label: (slug: string) => string = (slug) => slug,
): string[] {
  const onDisk = new Set(onMachine.map((name) => name.toLowerCase()));
  const sole = identity.project_repos.length === 1;
  const lines: string[] = [];
  for (const entry of identity.project_repos) {
    // With a project settled, the others are somebody else's business.
    if (project !== null && entry.project_slug !== project) continue;
    const absent = entry.repos.filter((repo) => !onDisk.has(repo.toLowerCase()));
    if (absent.length === 0) continue;
    const shown =
      absent.length > 10
        ? `${absent.slice(0, 10).join(", ")} and ${absent.length - 10} more`
        : absent.join(", ");
    const where =
      sole || entry.project_slug === project
        ? "this project"
        : `project ${label(entry.project_slug)}`;
    lines.push(`Also in ${where}, not on this machine: ${shown}.`);
  }
  return lines;
}

/** A list as a sentence names it, capped: "a, b and 3 more". */
function listed(items: string[], cap: number): string {
  if (items.length <= 1) return items.join("");
  if (items.length > cap) return `${items.slice(0, cap).join(", ")} and ${items.length - cap} more`;
  return `${items.slice(0, -1).join(", ")} and ${items.at(-1)}`;
}

/** What the setup line says was set up, and where. */
export type SetupState = {
  /** The repos covered, as the reader knows them. */
  repos: string[];
  workspace: string;
  /** The project, as printed, or null when none was settled. */
  project: string | null;
  /** The agent hosts whose hook file this run holds. */
  hooks: string[];
  skills: boolean;
  /** The clients that now have the Carrick MCP server. */
  mcp: string[];
};

/**
 * The one line that says what is set up (carrick#1489).
 *
 * It used to be nine blocks: the login, the connection, the packages and the
 * proposal path, each hook file, the skills, the MCP clients, the index. The
 * files were named in the question that asked to write them, so what is left
 * to say is the state.
 */
export function setupLine(state: SetupState): string {
  const subject = state.repos.length === 1 ? state.repos[0] : `${state.repos.length} repos`;
  const where = state.project === null ? state.workspace : `${state.workspace} · project ${state.project}`;
  const parts = [
    ...(state.hooks.length > 0 ? [`${state.hooks.join(" and ")} hooks`] : []),
    ...(state.skills ? ["task skills"] : []),
    ...(state.mcp.length > 0 ? [`MCP in ${state.mcp.join(", ")}`] : []),
  ];
  const head = state.repos.length === 0 ? `Set up in ${where}` : `Set up ${subject} in ${where}`;
  return parts.length === 0 ? head : `${head}: ${parts.join(", ")}`;
}

/**
 * The installs a first scan would be refused over, as commands to run.
 *
 * The scanner states them (`not_installed` on each derived repo, from the same
 * preflight rule the scan refuses on), so this only says where each runs, as
 * a path from the workspace the agent is in (carrick#1489).
 */
export function installCommands(plan: WorkspaceProposal): string[] {
  const commands: string[] = [];
  for (const repo of plan.repos) {
    for (const row of repo.not_installed) {
      const where = path.join(path.relative(plan.workspace, repo.path), row.directory);
      const command = where === "." ? `\`${row.command}\`` : `\`${row.command}\` in ${where}`;
      if (!commands.includes(command)) commands.push(command);
    }
  }
  return commands;
}

/**
 * The sentence that installs them, when there is anything to install: an
 * instruction to the agent, or a line to the reader when no instruction is
 * printed.
 */
export function installSentence(commands: string[], to: "agent" | "reader"): string | null {
  if (commands.length === 0) return null;
  return to === "agent"
    ? `First install dependencies: run ${listed(commands, 5)}.`
    : `Dependencies are not installed. Run ${listed(commands, 5)} before the next scan.`;
}

/** A path as a reader types it, with the home directory as `~`. */
export function tilde(target: string, home: string = os.homedir()): string {
  if (target === home) return "~";
  return target.startsWith(`${home}${path.sep}`) ? `~${target.slice(home.length)}` : target;
}

/**
 * The files the write step creates, as one line of the list "Go ahead?" is
 * asked about (carrick#1489, carrick#1512).
 *
 * Directories inside the workspace, because each holds several files and the
 * list has to fit one line; every file outside it by its own path, because
 * those belong to somebody's editor. Claude Code's server list is written by
 * its own command, so it is named rather than given a path. It says "add",
 * because that is what happens: every writer here merges, and a line that
 * listed folders read as though it replaced them (carrick#1512).
 */
export function writesLine(options: { workspaceFile: boolean; editorFiles: string[]; claudeCode: boolean; home?: string }): string {
  const mcp = [
    ...(options.claudeCode ? ["Claude Code"] : []),
    ...options.editorFiles.map((file) => tilde(file, options.home)),
  ];
  const parts = [
    "Add Carrick's hooks and skills to .claude/, .agents/ and .codex/",
    options.workspaceFile ? `create .carrick/ and ${WORKSPACE_FILE}` : "create .carrick/",
    ...(mcp.length > 0 ? [`add Carrick's MCP server to ${listed(mcp, 5)}`] : []),
  ];
  return `${parts.slice(0, -1).join(", ")}, and ${parts.at(-1)}`;
}

/**
 * The GitHub App line of that list: which repos still need the App.
 *
 * With a terminal, init opens the page itself once the reader says yes, so the
 * line says that and no address. Without one it cannot, and the line carries
 * the address instead: it is the only place a run with no terminal says it.
 */
export function connectItem(unconnected: string[], total: number, interactive: boolean, url: string): string {
  const which =
    unconnected.length === total && total === 2
      ? "both repos"
      : unconnected.length === total && total > 2
        ? `all ${total} repos`
        : reposPhrase(unconnected);
  return interactive
    ? `Open GitHub to install Carrick on ${which}`
    : `Install the Carrick GitHub App on ${which}: ${url}`;
}

/**
 * Everything the run will do, one line each, for the one question that covers
 * it (carrick#1512).
 *
 * A project used to be created at its own question, before the reader had seen
 * which repos went into it, and the GitHub App line was printed twice around
 * the files question. Now the project, the App and the files are three lines
 * under "Next:", and "Go ahead?" is asked once, about all of them. A move out
 * of another project is a line here too, and still its own question after this
 * one, because `--yes` never grants it (carrick#1338).
 */
export function nextLines(input: {
  /** The name of a project this run creates, or null. */
  create: string | null;
  /** The project the repos go to, as printed, or null when none was settled. */
  project: string | null;
  /** The repos this run covers, as the reader knows them. */
  repos: string[];
  /** Repos that go into an existing project once the App is on them. */
  joining: string[];
  moving: Array<{ repo: string; from: string }>;
  /** The GitHub App line, or null when every repo is connected. */
  connect: string | null;
  writes: string;
}): string[] {
  const lines: string[] = [];
  if (input.create !== null) lines.push(`Create project ${input.create} with ${reposPhrase(input.repos)}`);
  else if (input.project !== null && input.joining.length > 0) {
    lines.push(`Add ${reposPhrase(input.joining)} to project ${input.project}`);
  }
  if (input.project !== null) {
    const sources = [...new Set(input.moving.map((entry) => entry.from))];
    for (const from of sources) {
      const repos = input.moving.filter((entry) => entry.from === from).map((entry) => entry.repo);
      lines.push(`Move ${reposPhrase(repos)} from ${from} into ${input.create ?? input.project}`);
    }
  }
  if (input.connect !== null) lines.push(input.connect);
  lines.push(input.writes);
  return lines;
}

/**
 * The line a run inside one repo prints about the others beside it, where it
 * does not ask about them: too many to list, or no terminal to ask in.
 */
export function thisRepoOnly(repo: string): string {
  return `Setting up ${repo} only. To set up several repos as one system, run carrick init in the folder that holds them.`;
}

/**
 * Which of the repos beside this one belong to the same system, asked in the
 * run itself (carrick#1512).
 *
 * The run used to name the folder above and tell the reader to start again
 * there. This repo starts ticked and the others do not: a sibling folder is
 * not a claim that the repos in it call each other. Each row names the GitHub
 * repo its remote points at, which is what the reader recognises when two
 * folders are called something else.
 */
export async function chooseSiblings(
  current: RepoIdentity,
  siblings: RepoIdentity[],
  directory: string,
  out: InitOutput,
): Promise<string[]> {
  const rows: Choice[] = [current, ...siblings].map((repo) => ({
    value: repo.path,
    label: repo === current ? `${path.basename(repo.path)} (this repo)` : path.basename(repo.path),
    hint: repo.name ?? "no GitHub remote",
  }));
  return out.choose(
    `${tilde(directory)} holds other repos. Which belong to the same system as ${path.basename(current.path)}?`,
    "a repo",
    rows,
    { initial: [current.path], required: true },
  );
}

/**
 * An end to the run with its own exit code, decided before anything is written.
 *
 * A refusal ("no terminal and no --yes") and a declined proposal are the same
 * shape — stop here, leave everything alone — and they happen at four points
 * in the decision. Thrown, they all end in one place, beside the cancel, which
 * is the only way to be sure no path between them reaches a write.
 */
class Stop extends Error {
  readonly code: number;
  constructor(code: number, message = "") {
    super(message);
    this.name = "Stop";
    this.code = code;
  }
}

/** The line a run that was stopped at a question ends on (carrick#1338). */
export const CANCELLED = "Cancelled. Nothing was written, here or in Carrick.";

/**
 * The line a declined proposal ends on.
 *
 * A "no" used to be silent, which beside a cancel that says so reads as two
 * different things having happened. Both leave the machine and the server as
 * they were, and both say it.
 */
export const NOTHING_WRITTEN = "Nothing written, here or in Carrick.";

/**
 * The repos that start selected, and why each of the others does not.
 *
 * A default is a claim about somebody's folder, so only the repos something
 * already says belong here carry one: connected to this workspace, and in the
 * project most of this folder's connected repos are in. Everything else — a
 * repo nobody connected, one sitting in a different project, one with no
 * GitHub identity to ask about — starts out, with the reason on its row, and
 * the reader adds it in one keystroke (carrick#1365).
 *
 * Two cases preselect nothing at all, and neither is in the ruling because
 * neither is a majority: a folder where nothing is connected (a first run), and
 * a tie between two projects. A guess there is a guess about which half of
 * somebody's folder this install is for.
 *
 * Unconnected is not the same as unknown: with no read of the workspace — no
 * login yet, or a read that did not answer — `identity` is null and nothing is
 * preselected, because "not connected" would then be this command's assumption
 * rather than the server's answer.
 */
export function preselectedRepos(
  candidates: RepoIdentity[],
  identity: ResolvedRepos | null,
  label: (slug: string) => string = (slug) => slug,
): { keep: string[]; reasons: Map<string, string> } {
  const reasons = new Map<string, string>();
  const project = new Map<string, string | null>();
  for (const repo of candidates) {
    if (repo.name === null) {
      reasons.set(repo.path, "no GitHub identity");
      continue;
    }
    if (identity === null) continue;
    const row = identity.repos.find(
      (entry) => entry.full_name.toLowerCase() === repo.name?.toLowerCase(),
    );
    if (row === undefined || !row.connected) {
      reasons.set(repo.path, "not connected");
      continue;
    }
    project.set(repo.path, row.project_slug);
  }
  const counts = new Map<string, number>();
  for (const slug of project.values()) {
    if (slug !== null) counts.set(slug, (counts.get(slug) ?? 0) + 1);
  }
  const ranked = [...counts.entries()].sort((left, right) => right[1] - left[1]);
  const majority = ranked.length > 0 && (ranked.length === 1 || ranked[0]![1] > ranked[1]![1]) ? ranked[0]![0] : null;
  const keep: string[] = [];
  for (const [repoPath, slug] of project) {
    if (majority !== null && slug === majority) keep.push(repoPath);
    else if (slug === null) reasons.set(repoPath, "connected, in no project");
    else reasons.set(repoPath, `in ${label(slug)}`);
  }
  return { keep, reasons };
}

/** A repo as the picker lists it: what it is called, what is in it, and why it is out. */
export function repoChoices(
  plan: WorkspaceProposal,
  candidates: RepoIdentity[],
  reasons: Map<string, string> = new Map(),
): Choice[] {
  return candidates.map((repo) => {
    const found = plan.repos.find((entry) => entry.path === repo.path);
    const packages = found?.services.length ?? 0;
    const reason = reasons.get(repo.path);
    const hint = [
      `${packages} package${packages === 1 ? "" : "s"}`,
      ...(reason === undefined ? [] : [reason]),
    ].join(", ");
    return { value: repo.path, label: repo.name ?? path.basename(repo.path), hint };
  });
}

/**
 * Which repos in this folder this install covers, settled before anything is
 * read from Carrick and long before anything is written (carrick#1338).
 *
 * A folder routinely holds a repo that must not be scanned, and the only
 * answer this command used to take was one yes covering every repo in it. The
 * terminal picks; without one, `--repo` names them; with neither, there is no
 * safe default and the run stops here, having written nothing.
 */
export async function chooseRepos(
  plan: WorkspaceProposal,
  candidates: RepoIdentity[],
  options: {
    repos: string[];
    assumeYes: boolean;
    interactive: boolean;
    /** The workspace read that decides the defaults, or null when there is none. */
    identity?: ResolvedRepos | null;
    label?: (slug: string) => string;
  },
  out: InitOutput,
): Promise<RepoIdentity[]> {
  if (options.repos.length > 0) {
    const chosen = selectRepos(candidates, options.repos);
    if ("problem" in chosen) throw new Error(chosen.problem);
    if (chosen.taken !== null) {
      out.done(`${chosen.taken.name} taken as the GitHub repository for ${chosen.taken.path}`);
    }
    return chosen.repos;
  }
  // One repo is not a choice, and `--yes` is a reader stating that the list as
  // derived is the list they want.
  if (candidates.length === 1 || options.assumeYes) return candidates;
  if (!options.interactive) {
    throw new Error(
      `${plan.workspace} holds ${candidates.length} repos and there is no terminal to choose in. Name the ones this install covers with --repo owner/repo (repeatable), or add --yes to cover all ${candidates.length}.`,
    );
  }
  const { keep, reasons } = preselectedRepos(candidates, options.identity ?? null, options.label);
  const kept = new Set(
    await out.choose(
      "Which repos should Carrick index?",
      "a repo",
      repoChoices(plan, candidates, reasons),
      { initial: keep, required: true },
    ),
  );
  return candidates.filter((repo) => kept.has(repo.path));
}

/**
 * The excluded repos a `--repo` flag is asking for, by identity.
 *
 * An excluded repo is invisible to everything downstream — the scanner never
 * derives it, so `selectRepos` sees a value matching nothing on disk and, by
 * carrick#991's rule, attaches it to the one repo here with no GitHub identity
 * of its own. That is a flag quietly covering a different repository, so the
 * question is asked here instead, before any of it.
 *
 * Asked of the directory, not of the spelling: `--repo owner/web` and a
 * directory called `web` are not the same claim, and the repo's own origin
 * remote is what says whether they name one repository. A directory that is
 * gone, or that names no GitHub repo, answers nothing and is not reported —
 * there is no identity to have asked for.
 */
export function namedButExcluded(
  workspace: string,
  excluded: string[],
  requested: string[],
  identify: (repo: string) => RepoIdentity = repoIdentity,
): string[] {
  if (requested.length === 0 || excluded.length === 0) return [];
  const wanted = new Set(requested.map((name) => name.toLowerCase()));
  return excluded.filter((name) => {
    const directory = path.join(workspace, name);
    if (!fs.existsSync(directory)) return false;
    const identity = identify(directory);
    return identity.name !== null && wanted.has(identity.name.toLowerCase());
  });
}

/**
 * Which editors outside the workspace get an MCP entry written for them.
 *
 * The answer is a set of names, and the default is the empty one: these are
 * files in somebody's home directory for editors they may not use, so the
 * strongest thing a configuration directory can do is put a row in the
 * question (carrick#1365). An editor whose own command answers on PATH starts
 * ticked; without a terminal, `--mcp` names them and nothing else is written.
 *
 * `--yes` does not cover these. It takes the proposal as printed — a list of
 * packages and a set of files inside this workspace — and an entry in
 * `~/.cursor/mcp.json` is neither.
 */
export async function chooseEditors(
  offered: OfferedClient[],
  options: { editors: string[]; assumeYes: boolean; interactive: boolean },
  out: InitOutput,
): Promise<string[]> {
  if (offered.length === 0) return [];
  if (options.editors.length > 0) {
    const known = new Map(offered.map((client) => [client.name.toLowerCase(), client.name]));
    const chosen: string[] = [];
    for (const wanted of options.editors) {
      const name = known.get(wanted.toLowerCase());
      if (name === undefined) {
        throw new Error(
          `--mcp ${wanted} names no editor configured on this machine. The ones here are: ${offered.map((client) => client.name).join(", ")}.`,
        );
      }
      if (!chosen.includes(name)) chosen.push(name);
    }
    return chosen;
  }
  if (!options.interactive || options.assumeYes) return [];
  return out.choose(
    "Which editors should Carrick be added to? These files are outside this workspace.",
    "an editor",
    offered.map((client) => ({
      value: client.name,
      label: client.name,
      hint: client.installed ? client.file : `${client.file}, not detected on this machine`,
    })),
    { initial: offered.filter((client) => client.installed).map((client) => client.name), required: false },
  );
}

export async function init(argv: string[]): Promise<number> {
  return initWith(argv, createOutput(), process.stdin.isTTY === true);
}

/**
 * `init` with the terminal stated rather than sniffed, so a test can be the
 * terminal: the cancel path and the picker have no other way in.
 */
export async function initWith(argv: string[], out: InitOutput, interactive: boolean): Promise<number> {
  const parsed = parseArgs(argv);
  if (typeof parsed === "string") {
    process.stdout.write(`${parsed}\n`);
    return parsed.startsWith("carrick init") ? 0 : 2;
  }
  // Where the run was started, and where it sets up: the folder above, once
  // the reader has said a repo beside this one belongs with it (carrick#1512).
  const startedIn = parsed.workspace;
  let workspace = parsed.workspace;

  if (!fs.existsSync(workspace)) {
    process.stderr.write(`carrick init: ${workspace} is not a directory on this machine\n`);
    return 1;
  }

  // Everything this run would change is decided before any of it happens: the
  // repos it covers, the project they belong in, and the moves that would
  // take. `--project` used to move repos on the server as its first act and
  // print the proposal afterwards, so a reader's only chance to drop a repo
  // came after that repo had already been moved (carrick#1338).
  //
  // The repos the workspace read found an index for: where CI has already built
  // one, this run reads it rather than ordering a scan (carrick#993 row 2).
  const hostedIndex: string[] = [];
  // The project the skills are written against, once it is settled. It stays
  // null where no repo here names a GitHub repository, and the skills then
  // tell the agent to read the scope from the git remote instead.
  let projectSlug: string | null = null;
  let derived: DerivedWorkspace;
  let credential: Credential;
  let names: string[];
  let initial: ResolvedRepos;
  let projects: Project[] | null;
  let decision: ProjectChoice;
  // The repos this run has permission to take out of another project.
  const movable = new Set<string>();
  // The ones among them that sit in a project somebody chose, at the read the
  // proposal was built from. Their move is said; a repo the App grant has
  // just put in the default project is not (carrick#1489).
  const announced = new Set<string>();
  // The directory names this run leaves out, for the workspace file below.
  let deselected: string[] = [];
  // The editors outside this workspace the answer covers, settled before it.
  let editors: string[] = [];
  // The repos this install covers, for the closing line that names what is
  // still to set up in them.
  let selectedRepos: RepoIdentity[] = [];
  try {
    // What a previous run wrote down, before anything is derived from it: the
    // scanner has already dropped an excluded repo by the time the proposal
    // arrives, so this is the only place that can tell a reader why the repo
    // they just named is not here (carrick#1344).
    const excluded = excludedRepos(workspace);
    const asked = namedButExcluded(workspace, excluded, parsed.repos);
    if (asked.length > 0) {
      throw new Error(
        `${WORKSPACE_FILE} leaves ${asked.join(" and ")} out of this workspace, so --repo cannot cover ${asked.length === 1 ? "it" : "them"}. Take ${asked.length === 1 ? "that name" : "those names"} out of the exclude list there and run carrick init again.`,
      );
    }
    if (excluded.length > 0) {
      out.say(
        `${WORKSPACE_FILE} leaves ${excluded.join(", ")} out of this workspace, so nothing below covers ${excluded.length === 1 ? "it" : "them"}.`,
      );
    }
    // The derivation and the identities are local reads: they name the choice,
    // and a run nobody has chosen in yet must not have signed anything in.
    derived = deriveWorkspace(workspace);
    let candidates = derived.plan.repos.map((repo) => repoIdentity(repo.path));
    // The repos beside this one, asked about here rather than named with an
    // instruction to start again in the folder above (carrick#1512). A yes to
    // any of them carries on as if the run had started there: the same
    // derivation, the same proposal, and the repos left unticked written to
    // the same exclude list a folder run writes.
    let ticked: RepoIdentity[] | null = null;
    const parent = derived.plan.parent_proposal;
    if (parent !== null && candidates.length === 1) {
      const current = candidates[0]!;
      const siblings = siblingRepos(derived.plan, excludedRepos(parent.directory));
      const asking =
        siblings.length > 0 &&
        siblings.length <= MAX_SIBLINGS &&
        interactive &&
        !parsed.assumeYes &&
        parsed.repos.length === 0;
      if (asking) {
        const picked = new Set(
          (await chooseSiblings(current, siblings.map((repo) => repoIdentity(repo)), parent.directory, out)).map(realPath),
        );
        if (picked.size > 1 || !picked.has(realPath(current.path))) {
          workspace = parent.directory;
          derived = deriveWorkspace(workspace);
          candidates = derived.plan.repos.map((repo) => repoIdentity(repo.path));
          const chosen = candidates.filter((repo) => picked.has(realPath(repo.path)));
          // This repo first, where the reader kept it: it is the one they
          // started in, and every sentence below names it first.
          ticked = [
            ...chosen.filter((repo) => realPath(repo.path) === realPath(current.path)),
            ...chosen.filter((repo) => realPath(repo.path) !== realPath(current.path)),
          ];
        }
      } else if (siblings.length > 0) {
        out.say(thisRepoOnly(path.basename(current.path)));
      } else if (parent.repos.some((repo) => realPath(repo) === realPath(parent.directory))) {
        // Not a folder of repos but a repository this directory sits inside,
        // which is where a scan of it belongs: the one case where "run it in
        // the folder above" is still the answer.
        out.warn(`The parent folder ${parent.directory} is itself a repo. Run carrick init .. to initialise that workspace.`);
      }
    }
    // What Carrick already knows about this folder, before the picker draws
    // its defaults. A read, not a write: the credential has to exist already,
    // so a machine nobody has signed in on still chooses first and signs in
    // afterwards, which is what keeps "nothing written, here or in Carrick"
    // true of a cancelled run (carrick#1338). A read that does not answer
    // preselects nothing rather than guessing, and the authoritative read
    // below then reports the failure in its own words.
    const saved: Credential | null = readCredential();
    let known: ResolvedRepos | null = null;
    projects = null;
    const knownNames = [...new Set(candidates.map((repo) => repo.name).filter((name): name is string => name !== null))];
    // Only where a picker is actually going to draw. Every other path has its
    // answer already — `--repo` names it, `--yes` takes the list as derived, a
    // single repo is not a choice, the question about the repos beside this
    // one was the choice — and asking about a repo whose name the reader never
    // offered is a request that buys nothing (carrick#1338's test pins it: a
    // repo left out is named in no request at all).
    const picking =
      ticked === null && parsed.repos.length === 0 && !parsed.assumeYes && interactive && candidates.length > 1;
    if (picking && saved !== null && knownNames.length > 0) {
      try {
        known = await resolveRepos(saved.token, knownNames);
        projects = await listProjects(saved.token);
      } catch {
        known = null;
        projects = null;
      }
    }
    const selected =
      ticked ??
      (await chooseRepos(
        derived.plan,
        candidates,
        {
          repos: parsed.repos,
          assumeYes: parsed.assumeYes,
          interactive,
          identity: known,
          label: (slug) => projectLabel(slug, projects),
        },
        out,
      ));
    if (selected.length === 0) throw new Stop(0);
    selectedRepos = selected;
    derived = selectedProposal(derived, selected.map((repo) => repo.path));
    const kept = new Set(selected.map((repo) => repo.path));
    deselected = candidates
      .filter((repo) => !kept.has(repo.path))
      .map((repo) => path.basename(repo.path));
    const dropped = candidates.length - selected.length;
    // Not after the question about the repos beside this one: its rows were
    // the whole answer, and a folder repo that had no row there (one with no
    // JavaScript or TypeScript in it) is not news.
    if (dropped > 0 && ticked === null) {
      out.done(
        `${selected.length} of ${candidates.length} repos covered; ${dropped} left out of the proposal, the project and the connection`,
      );
    }
    // A machine that has never signed in signs in here rather than being told
    // to run another command: `carrick login` is the same browser round trip,
    // and refusing to take it was the first thing a new install did
    // (carrick#955). Without a terminal there is no browser to hand it to.
    if (saved) credential = saved;
    else {
      if (!interactive) {
        throw new Error("carrick init requires a Carrick login. Run carrick login, or set CARRICK_TOKEN.");
      }
      credential = await signIn(out.say);
    }
    names = [...new Set(selected.map((repo) => repo.name).filter((name): name is string => name !== null))];
    if (names.length > 200) throw new Error("This workspace has more than 200 GitHub repos. Initialise smaller workspace groups.");
    const missingIdentities = selected.filter((repo) => repo.name === null);
    // Said before anything is requested, and said whether or not --project was
    // given: a repo with no identity is left out of the project step, the
    // connection check and the workspace read, and a run that dropped it in
    // silence read as a complete one (carrick#991).
    // A folder of clones can hold dozens, so the list is capped and the rest
    // is counted rather than dropped.
    for (const repo of missingIdentities.slice(0, 10)) {
      out.warn(`${repo.path} contributes no GitHub identity: ${repo.problem}.`);
    }
    if (missingIdentities.length > 10) {
      out.warn(`${missingIdentities.length - 10} more repos here name no GitHub identity either.`);
    }
    if (missingIdentities.length > 0) {
      const one = missingIdentities.length === 1;
      out.warn(
        `Carrick has nothing to connect ${one ? "it" : "them"} to. Run carrick init --repo owner/repo${one ? "" : " in each of them"}, or give the alias a HostName github.com line in your ssh config.`,
      );
    }
    if (parsed.project && missingIdentities.length > 0) {
      throw new Error(
        `--project ${parsed.project} cannot verify ${missingIdentities.map((repo) => repo.path).join(", ")} because ${missingIdentities.length === 1 ? "it has" : "they have"} no GitHub origin. No project assignment was verified.`,
      );
    }
    if (parsed.project && names.length === 0) {
      throw new Error(`--project ${parsed.project} found no GitHub repos to verify.`);
    }
    if (names.length === 0) {
      // The sentence that stops the rest of this run from reading as a
      // complete one: no project is chosen, no connection is checked, and a
      // later upload has no repo identity to resolve a project from.
      out.warn("No repo here names a GitHub repository, so this run chooses no project and checks no connection.");
    }
    // The read the rest of this run is decided from. The one the picker's
    // defaults came from is reused only where the selection kept every repo,
    // because `project_repos` answers for the repos that were ASKED about: a
    // wider read would have the absent-repo lines below naming projects this
    // install does not cover (carrick#993 row 18).
    const everyCandidate = known !== null && names.length === knownNames.length;
    initial = everyCandidate && known !== null ? known : await resolveRepos(credential.token, names);
    // Read once, for the names: every project this run prints is printed as
    // the dashboard shows it, with the slug beside it (carrick#1338). A
    // workspace whose API has no such action answers null and the lines carry
    // slugs alone, as they did.
    if (projects === null) projects = names.length > 0 ? await listProjects(credential.token) : null;
    // The project half of the browser round trip, where this API can do it
    // from here. Nothing is created or moved in it: it settles what this run
    // would do, and the proposal below is where that is accepted. Without
    // --project the step reads the assignment the repos already have and
    // offers the list, rather than doing nothing at all (carrick#987).
    const dashboard = workspaceUrls(initial.workspace.slug).projects;
    // The repos by the folder names the reader has on disk, in the order the
    // questions name them.
    const shown = selected.map((repo) => path.basename(repo.path));
    const shortName = (fullName: string): string => {
      const repo = selected.find((entry) => entry.name?.toLowerCase() === fullName.toLowerCase());
      return repo === undefined ? fullName : path.basename(repo.path);
    };
    const prompts: ProjectPrompts = {
      repos: shown,
      say: out.say,
      ask: out.ask,
      confirm: out.confirm,
      pick: out.pick,
      dashboard: { url: dashboard, open: (url) => void openBrowser(url).catch(() => false) },
      interactive,
      assumeYes: parsed.assumeYes,
    };
    decision =
      parsed.project === null
        ? await projectStep(projectAssignments(initial, names), projects, prompts)
        : reposAreInProject(initial, names, parsed.project)
          ? { slug: parsed.project, exists: true, create: false }
          : await planProject(parsed.project, projects, prompts);
    const project = decision.slug;
    projectSlug = project;
    const label = (slug: string): string => projectLabel(slug, projects);
    // What the server would be asked to change, before it is asked: the repos
    // that would leave a project somebody put them in, and the ones the grant
    // has yet to connect at all.
    const current = projectAssignments(initial, names);
    const assigned = new Map(names.map((name, index) => [name, current[index] ?? null]));
    const moving = project === null ? [] : names.filter((name) => {
      const current = assigned.get(name);
      return current !== null && current !== undefined && current !== project;
    });
    const joining = project === null ? [] : names.filter((name) => assigned.get(name) == null);

    // The proposal: what this run covers, what it would change, and what the
    // scanner said about it. Everything above is a read; everything below the
    // confirm is a write.
    const plan = derived.plan;
    // The scanner's own warnings about what it proposed. Capped: the proposal
    // file carries every one of them, and it is named on the line above.
    const warnings = plan.repos.flatMap((repo) => repo.warnings);
    for (const warning of warnings.slice(0, 3)) out.warn(warning);
    if (warnings.length > 3) out.warn(`${warnings.length - 3} more notes on the proposal are in ${PROPOSAL_FILE}.`);
    for (const missing of plan.missing) out.warn(`Missing workspace override: ${missing}`);
    // Which editors get an entry, asked before the yes that covers it. The
    // files are outside the workspace and belong to editors this reader may not
    // use, and they used to be written on the strength of a yes to the proposal
    // (carrick#1365). An editor with no configuration directory is not offered,
    // and one this machine has no command for is offered unticked.
    const offered = offeredFileClients();
    editors = await chooseEditors(offered, { editors: parsed.editors, assumeYes: parsed.assumeYes, interactive }, out);
    // What the yes is given to: every thing this run will do, one line each —
    // the project, the GitHub App, the files — and nothing created before it
    // (carrick#1512). The files are named, editor files included, because a
    // question that does not name a file is not consent to write it
    // (carrick#1365, carrick#1489).
    const unconnected = unconnectedRepos(initial, names);
    const next = nextLines({
      create: decision.create && project !== null ? (decision.name ?? project) : null,
      project: project === null ? null : label(project),
      repos: shown,
      joining: joining.map(shortName),
      moving: moving.map((name) => ({ repo: shortName(name), from: label(assigned.get(name) ?? "") })),
      connect:
        unconnected.length === 0
          ? null
          : connectItem(
              unconnected.map(shortName),
              names.length,
              interactive,
              workspaceUrls(initial.workspace.slug).connect,
            ),
      writes: writesLine({
        workspaceFile: deselected.length > 0,
        editorFiles: offered.filter((client) => editors.includes(client.name)).map((client) => client.file),
        claudeCode: claudeCodeFound(),
      }),
    });
    out.say(["Next:", ...next.map((line) => `  ${line}`)].join("\n"));
    if (!parsed.assumeYes) {
      if (!interactive) {
        throw new Stop(
          1,
          `use --yes to accept this proposal without a terminal${moving.length > 0 && !parsed.allowMove ? ", and --allow-move to accept the move above" : ""}.`,
        );
      }
      if (!(await out.confirm("Go ahead?"))) {
        out.refuse(NOTHING_WRITTEN);
        throw new Stop(0);
      }
    }
    // A move is its own question. `--yes` accepts a list of packages and a set
    // of local files; taking a repo out of a project changes what every agent
    // querying that project can see, and that is not the same answer
    // (carrick#1338).
    if (moving.length > 0 && project !== null && !parsed.allowMove) {
      const from = [...new Set(moving.map((name) => label(assigned.get(name) ?? "")))].join(" and ");
      if (!interactive) {
        throw new Stop(
          1,
          `${moving.join(", ")} ${moving.length === 1 ? "is" : "are"} in project ${from}. Moving ${moving.length === 1 ? "it" : "them"} into ${label(project)} changes what every agent querying either project can see, so it needs --allow-move.`,
        );
      }
      if (!(await out.confirm(`Move ${moving.length === 1 ? moving[0] : `${moving.length} repos`} out of ${from} into ${label(project)}?`))) {
        out.refuse(NOTHING_WRITTEN);
        throw new Stop(0);
      }
    }
    for (const name of [...moving, ...joining]) movable.add(name.toLowerCase());
    for (const name of moving) announced.add(name.toLowerCase());
  } catch (error) {
    // A question the reader ended stops the run where it stands. Nothing above
    // this line writes, so there is nothing to undo (carrick#1338).
    if (error instanceof PromptCancelled) {
      out.refuse(CANCELLED);
      return 1;
    }
    if (error instanceof Stop) {
      if (error.message !== "") process.stderr.write(`carrick init: ${error.message}\n`);
      return error.code;
    }
    process.stderr.write(`carrick init: ${(error as Error).message}\n`);
    return 1;
  }

  // Accepted. From here the run writes: the project, the assignment, the
  // proposal, the hooks, the skills and the MCP entries.
  const plan = derived.plan;
  const project = decision.slug;
  const label = (slug: string): string => projectLabel(slug, projects);
  // What the reader still has to do, said once at the end rather than between
  // the steps (carrick#1489).
  const todo: string[] = [];
  let workspaceSlug = initial.workspace.slug;
  let projectExists = decision.exists;
  try {
    if (decision.create && project !== null) {
      const outcome = await createProject(credential.token, project, decision.name ?? project);
      if (outcome.kind === "created") {
        // Known by its name from here on, the same as a listed one.
        projects = [...(projects ?? []), outcome.project];
        // A workspace's first project takes the repos the GitHub App install
        // staged (carrick-cloud#1359); any other starts empty.
        const holds = outcome.project.repo_count;
        out.done(`Created project ${label(project)}${holds > 0 ? ` with ${holds} repo${holds === 1 ? "" : "s"}` : ""}`);
        projectExists = true;
      } else if (outcome.kind === "refused") {
        out.warn(`Carrick did not create ${label(project)}: ${outcome.message}`);
      }
    }
  } catch (error) {
    process.stderr.write(`carrick init: ${(error as Error).message}\n`);
    return 1;
  }
  try {
    // The proposal is a seed for an agent, not a config: nothing derived
    // without a model is written into the repository, because the first scan
    // is the paid one and it has to run against a config someone has read
    // (carrick-cloud#799).
    // Both files were named in the question that asked to write them, so
    // neither gets a line of its own (carrick#1489).
    writeProposal(plan.workspace, derived);
    // And the selection itself, in the file every later command reads. Without
    // it the choice lasted one run: `carrick refresh` and `carrick index` walk
    // the folder, the editor hook answers for a file inside a repo nobody
    // covers, and the next `init` asks again (carrick#1344).
    writeSelection(plan.workspace, deselected);
  } catch (error) {
    process.stderr.write(`carrick init: ${(error as Error).message}\n`);
    return 1;
  }
  // The agent hooks, merged by command: this file may already hold a user's
  // own hooks, or the hook pack the hosted index installs. And the MCP
  // connection, for work that crosses repos this machine does not hold,
  // configured for every client this machine has rather than printed for one
  // of them (carrick#955).
  // Before the hook commands are decided, because what they say depends on the
  // answer: with a global, they are the bare `carrick`, which every shell and
  // every agent on this machine resolves; without one, they name this install,
  // and an npx cache is cleared (carrick#1372). A machine that already has one
  // was brought level before this command started.
  try {
    await offerGlobalInstall({
      assumeYes: parsed.assumeYes,
      install: parsed.installGlobal,
      confirm: (question) => out.confirm(question),
      say: (line) => out.warn(line),
    });
  } catch (error) {
    // Including a cancelled prompt: somebody answering Ctrl-C to an offer is
    // answering the offer, not ending the setup.
    if (!(error instanceof PromptCancelled)) {
      out.warn(`The global install was not offered: ${(error as Error).message}`);
    }
  }
  const command = hookCommand({ onPath: carrickOnPath });
  const settingsName = path.join(".claude", command.bare ? "settings.json" : "settings.local.json");
  const settingsFile = path.join(workspace, settingsName);
  let hooksWritten = true;
  try {
    const settings = fs.existsSync(settingsFile) ? fs.readFileSync(settingsFile, "utf8") : null;
    const otherFile = path.join(workspace, ".claude", command.bare ? "settings.local.json" : "settings.json");
    const other = fs.existsSync(otherFile) ? fs.readFileSync(otherFile, "utf8") : null;
    // The same removal `carrick remove` runs, so a file holding no entry of
    // ours is left byte for byte rather than reformatted on the way past.
    const cleaned = other === null ? null : removeCarrickHooks(other);
    const hooks = mergeCarrickHooks(settings, command.command);
    // Validate both documents before migrating our entries between them.
    if (cleaned?.changed) writeIfChanged(otherFile, cleaned.body);
    writeIfChanged(settingsFile, hooks.body);
  } catch (error) {
    hooksWritten = false;
    out.refuse(
      `Could not configure Carrick hooks: ${(error as Error).message}. Fix ${settingsName} and run carrick init again.`,
    );
  }
  // The same two-part nudge for Codex, in the file Codex reads project hooks
  // from. Written for both hosts for the same reason the task skills are
  // (carrick#1335): which agent opens this workspace is not a thing init can
  // know, and a hook file for a host nobody runs costs nothing.
  const hosts = hooksWritten ? ["Claude Code"] : [];
  try {
    if (writeCodexHooks(workspace, command.command) === "written") {
      todo.push("Codex asks you to trust the Carrick hooks the next time it starts; until you do, they do not run.");
    }
    hosts.push("Codex");
  } catch (error) {
    out.refuse(
      `Could not configure the Codex hooks: ${(error as Error).message}. Fix ${CODEX_HOOKS_FILE} and run carrick init again.`,
    );
  }

  // The task skills, beside the hooks and independent of them: a settings file
  // somebody hand-edited into invalid JSON is no reason to withhold the bodies
  // an agent reads. A file this package did not write, or one somebody has
  // since changed, is left alone and named.
  let skills = false;
  try {
    const { done, warn } = taskSkillLines(writeTaskSkills(workspace, { slug: projectSlug }));
    skills = done.length > 0;
    todo.push(...warn);
    // Which build wrote them, so a hook running an older `carrick` than this
    // one says so instead of answering from files it does not match
    // (carrick#1372).
    recordCliVersion(workspace, currentVersion());
    const ignored = ignoredSkillRoots(workspace);
    if (ignored.length > 0) {
      const one = ignored.length === 1;
      todo.push(
        `${ignored.join(" and ")} ${one ? "is" : "are"} git-ignored here, so these skills stay on this machine. Track ${one ? "it" : "them"} to give the rest of the team the same answers.`,
      );
    }
  } catch (error) {
    out.refuse(`Could not write the task skills: ${(error as Error).message}`);
  }

  // The same hooks and skills inside each repo of a folder, because an agent
  // started inside one of them reads neither host's files from the folder
  // above (carrick#1512, option A as ruled). Kept out of git through each
  // repo's own exclude file; a repo that is the workspace already has them.
  for (const repo of selectedRepos) {
    if (realPath(repo.path) === realPath(workspace)) continue;
    try {
      writeRepoCopy(repo.path, command.command, { slug: projectSlug });
    } catch (error) {
      out.refuse(`Could not add Carrick's hooks and skills to ${path.basename(repo.path)}: ${(error as Error).message}.`);
    }
  }

  // Nothing here about restarting Claude Code: the instruction this run closes
  // on is pasted into a new agent session, which loads the MCP server it has
  // just been given (carrick#1512).
  const mcp = connectMcpClients(editors);
  // Nothing here about `carrick` not being on PATH: the offer above said it, in
  // front of the decision it changes, and with the command for this machine's
  // package manager rather than a guess at npm (carrick#1372).
  for (const outcome of mcp.filter((entry) => entry.state === "failed")) {
    todo.push(`MCP not configured for ${outcome.client}: ${outcome.detail}`);
  }
  // Said as what is missing, not as "no agent client": the hooks above were
  // written for two of them (carrick#1489).
  if (!mcp.some((outcome) => outcome.client === "Claude Code")) {
    todo.push(`No claude command on this machine, so Claude Code has no Carrick MCP server. Once it is installed: ${mcpLine()}`);
  }

  // The connection, last of the steps that talk to Carrick, so a read that
  // fails costs nothing already decided: the files above are written
  // (carrick#1489 review).
  //
  // Read again, now. The read the questions were answered from can be minutes
  // old — the App is often installed in another tab while init waits at a
  // prompt — and a first project may have just taken the repos. The
  // connection lines used to come from the old read, and sent a reader to the
  // grant page for repos that were already connected (carrick#1489).
  try {
    const current = await resolveRepos(credential.token, names);
    const role = current.workspace.role;
    const identity = await connectRepos(credential.token, names, current, {
      interactive,
      project: project ?? undefined,
      projectExists,
      movable,
      announce: announced,
      label,
      // A role the read does not state is unknown, not a member's.
      member: role !== undefined && role !== "owner" && role !== "admin",
      say: out.say,
      track: (text, work, report) => out.step(text, work, report),
    });
    workspaceSlug = identity.workspace.slug;
    if (project !== null && !reposAreInProject(identity, names, project)) {
      // An unverified project does not end the run, whether it was named on
      // the command line or picked here. Stopping cost someone their hooks and
      // their proposal for a browser step they could only take afterwards, and
      // the documented command then needed two runs (carrick#993 row 8).
      todo.push(
        `Finish the browser steps above to put these repos in ${label(project)}, then run carrick init --project ${project} again to verify.`,
      );
    }
    // After a wait that was stopped, the grant is still the thing to do, and
    // the line that said so is above the wait.
    const unconnected = unconnectedRepos(identity, names);
    if (interactive && unconnected.length > 0) {
      todo.push(connectLine(unconnected, names.length, workspaceUrls(identity.workspace.slug).connect));
    }
    if (identity.allowance_sentence) out.say(identity.allowance_sentence);
    for (const repo of identity.repos) {
      if (repo.connected && repo.services.length > 0) hostedIndex.push(repo.full_name);
    }
    // The rest of the project, which this machine does not hold. Carrick
    // answers across every repo in a project, so a folder holding half of one
    // is a partial index and nothing here would otherwise say so
    // (carrick#993 row 18). "This machine" is every repo in the folder that
    // holds this run's repos, set up here or not: a repo in the folder beside
    // this one is not missing (carrick#1512). Only read where the run's own
    // repos leave something unaccounted for, because each is a git call.
    let absent = absentRepos(identity, names, project, label);
    if (absent.length > 0) {
      const folder = selectedRepos.some((repo) => realPath(repo.path) === realPath(workspace))
        ? path.dirname(workspace)
        : workspace;
      const here = repoRoots(folder)
        .map((root) => repoIdentity(root).name)
        .filter((name): name is string => name !== null);
      absent = absentRepos(identity, [...names, ...here], project, label);
    }
    todo.push(...absent);
  } catch (error) {
    // The server's own sentence says what failed; this one says what is left.
    todo.push(
      `${(error as Error).message} The files here are written; run carrick init again to verify the connection and the project.`,
    );
  }

  // What is set up, in one line; then the index; then what is left to do;
  // then the instruction for the agent (carrick#1489).
  out.done(
    setupLine({
      repos: selectedRepos.map((repo) => repo.name ?? path.basename(repo.path)),
      workspace: workspaceSlug,
      project: project === null ? null : label(project),
      hooks: hosts,
      skills,
      mcp: mcp.filter((outcome) => outcome.state !== "failed").map((outcome) => outcome.client),
    }),
  );

  // No paid scan ran here, and that is the point (carrick-cloud#799): the one
  // scan runs against a config someone has read. Where CI has already built an
  // index, this run reads it onto the machine instead — the hosted index is
  // the whole workspace's row, and `carrick index` from a laptop on a branch
  // replaces it for everyone who queries it (carrick#993 row 2, carrick#1020).
  if (hostedIndex.length > 0) {
    // The step reports its own outcome, so the read is one line in a terminal
    // as well as in a pipe: a spinner that stopped with its label spent a
    // second line on the same event (carrick#1032). What it says while it runs
    // is the scanner's own count of where it has got to (carrick#1365).
    //
    // The repo list goes in because the step decides for itself whether to
    // re-read this tree, and a repo the index does not cover is the one way
    // that decision cannot be read off a drift count (carrick#1373).
    const startedAt = Date.now();
    await out.step(
      STEP_LABEL,
      (progress) =>
        downloadHostedIndex(
          plan.workspace,
          nativeRunner(out.quiet, progress),
          plan.repos.map((repo) => repo.path),
          progress,
        ),
      (outcome) => hostedReport(outcome, (Date.now() - startedAt) / 1000),
    );
  }
  for (const line of todo) out.warn(line);
  // The scaffold tool writes the files a repo needs; a repo that has them needs
  // nothing pasted anywhere. The line used to close every run, including one
  // where every covered repo already had its config and its workflow, and
  // "this repo" named nothing in a folder of several (carrick#1365).
  //
  // An install the first scan would be refused over goes in front of it: the
  // agent otherwise found out from the refusal (carrick#1489).
  const installs = installCommands(plan);
  const unscaffolded = reposToScaffold(selectedRepos);
  if (unscaffolded.length > 0) {
    const install = installSentence(installs, "agent");
    // A new session, because that is the one that loads the MCP server this
    // run has just added (carrick#1512).
    out.note(NEXT_TITLE, [
      ...(install === null ? [] : [install]),
      scaffoldSentence(unscaffolded),
      // The scaffold tool reads the proposal from the folder init ran in, and
      // a run that moved to the folder above did not run where it started.
      ...(realPath(workspace) === realPath(startedIn) ? [] : [initFolderSentence(workspace)]),
    ]);
  } else {
    const install = installSentence(installs, "reader");
    if (install !== null) out.warn(install);
    out.done(READY_SENTENCE);
  }
  return 0;
}

/** `carrick templates <name>`, so a workflow is one command. */
export function templates(argv: string[]): number {
  const [name, ...rest] = argv;
  const variables: Record<string, string> = {};
  for (let index = 0; index < rest.length; index += 2) {
    const key = rest[index];
    const value = rest[index + 1];
    if (!key?.startsWith("--") || value === undefined) {
      process.stderr.write("carrick templates: expected --name value pairs\n");
      return 2;
    }
    variables[key.slice(2).toUpperCase().replace(/-/g, "_")] = value;
  }
  try {
    process.stdout.write(renderTemplate(name as never, variables));
    return 0;
  } catch (error) {
    process.stderr.write(`carrick templates: ${(error as Error).message}\n`);
    return 2;
  }
}
