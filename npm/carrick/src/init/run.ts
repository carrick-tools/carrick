// Authenticated setup of this machine for a workspace Carrick indexes.
//
// What this prints is one line per thing that happened (carrick#1026). The
// reasoning behind each step — how the hooks deliver, which editor extension
// to install — is in the docs, at the `DOCS*` links in `output.ts`, and this
// command links to them rather than repeating them. The record of the run
// stays here, because nothing else can state it: this login, this project,
// these repos, this index.
//
// There is one run (carrick#3361). Carrick holds each project's config and
// builds its index in the cloud, so nothing here proposes services, writes a
// config, creates or moves a project, or starts a scan: init reads which of
// these repos the workspace holds, installs the local tools for them, and
// reads their index in.

import { realPath } from "../fs-util.ts";
import fs from "node:fs";
import path from "node:path";
import { readCredential, type Credential } from "../auth/credentials.ts";
import { signIn } from "../auth/run.ts";
import { resolveRepos, type ResolvedRepos } from "../auth/read.ts";
import {
  deriveWorkspace,
  ensureIndexDir,
  keepRepos,
  selectRepos,
  siblingRepos,
  initFolderAbove,
  repoRoots,
  repoIdentity,
  type RepoIdentity,
  type WorkspaceProposal,
} from "./repos.ts";
import { reposUrl } from "./connect.ts";
import { downloadHostedIndex, hostedReport, nativeRunner, STEP_LABEL } from "./hosted.ts";
import { listProjects, projectLabel, SLUG, type Project } from "./projects.ts";
import { writeRepoCopy } from "./repo-copies.ts";
import { claudeCodeFound, connectMcpClients, mcpLine, offeredFileClients, type OfferedClient } from "./mcp.ts";
import { hookCommand, mergeCarrickHooks, removeCarrickHooks } from "./settings.ts";
import { CODEX_HOOKS_FILE, writeCodexHooks } from "./codex.ts";
import { AGENTS_FILE, writeAgentsSection } from "./agents-section.ts";
import { ignoredSkillRoots, taskSkillWarnings, writeTaskSkills } from "./task-skills.ts";
import { recordCliVersion } from "./outdated.ts";
import { findCarrick, offerGlobalInstall } from "../global-install.ts";
import { currentVersion } from "../update.ts";
import { writeIfChanged } from "./files.ts";
import { excludedRepos, readWorkspaceFile, removeSelection, writeSelection, WORKSPACE_FILE } from "./workspace-file.ts";
import {
  createOutput,
  DOCS_EDITOR,
  DOCS_INIT_FILES,
  listBlock,
  PromptCancelled,
  tilde,
  type InitOutput,
} from "./output.ts";
import { renderTemplate } from "../templates.ts";

/** What a run with nothing left to set up closes on: what this machine can do now. */
export const READY_SENTENCE =
  "The index is on this machine: `carrick check <file>` answers for a file, and your agent can ask who calls an operation.";

/** The title of the block a run closes on (carrick#3286). */
export const CLOUD_NEXT_TITLE = "Done. Open a new agent session in this folder, then ask it:";

/** The question the ready email also carries, so both say the same first thing. */
export const ASK_YOUR_AGENT_PROMPT =
  "Use Carrick to list every service in our project and the number of endpoints each one exposes";

/**
 * What a run closes on, under CLOUD_NEXT_TITLE: the first question to ask,
 * then only the steps still open. What init wrote is already listed while it
 * runs, and a failed step warns where it fails, so neither is repeated here.
 * Nobody scaffolds, so there is no instruction to paste (carrick#3286).
 */
export function cloudNextLines(codexTrust: boolean, indexPending: boolean): string[] {
  return [
    `${ASK_YOUR_AGENT_PROMPT}.`,
    ...(indexPending ? ["", "The index is not on this machine yet: `carrick refresh` reads it in once Carrick has built it."] : []),
    ...(codexTrust ? ["", "Codex: open `codex` here once and trust the project's hooks, or it skips them."] : []),
  ];
}

export type InitOptions = {
  workspace: string;
  /**
   * The project the repos must already be in. Repos change project on the
   * dashboard, so a slug that would move one stops the run; one they are in
   * changes nothing.
   */
  project: string | null;
  /**
   * The repos this install covers, `owner/repo` each, repeatable and
   * comma-separable. Empty means every repo here the workspace holds.
   *
   * It is also how a repository whose remote names none is named
   * (carrick#991): a value that matches nothing on disk attaches to the one
   * repo here that has no identity.
   */
  repos: string[];
  /**
   * The editors outside this workspace to add the MCP entry to, by name.
   * Empty means the ones whose own command answers on this machine.
   */
  editors: string[];
  /**
   * Install carrick globally where this machine has none (carrick#1372).
   *
   * Without a global, the hooks written below name this install — an npx
   * cache directory, when that is how this ran — and stop working when it is
   * cleared.
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
    installGlobal: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    switch (argument) {
      // Accepted and answered by nothing: init asks nothing after sign-in.
      // The release smoke, the release guard's init row and an eval script
      // pass it, so it is not refused (carrick#3361).
      case "--yes":
      case "-y":
        break;
      case "--install-global":
        options.installGlobal = true;
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
    "carrick init [DIRECTORY] [--repo OWNER/REPO]... [--mcp EDITOR] [--install-global]",
    "",
    "Sign in (here, or beforehand with carrick login), then set up a repository",
    "or a folder of repos for your agent: the agent hooks, the four task skills",
    "your agent loads, a short \"Carrick check\" section in AGENTS.md, and the MCP",
    "connection. It covers the repos here that your Carrick workspace holds, and",
    "reads the index Carrick has built for them into .carrick so this machine can",
    "answer from it. That read re-reads your source here first, which takes",
    "minutes on a large workspace. It skips the re-read when .carrick already holds",
    "an index current for this checkout. Carrick builds the index itself, in the",
    "cloud: init asks nothing after sign-in, runs no scan, and writes nothing into",
    "the repository but the ignored .carrick directory, the hook settings, the",
    "skills and that section.",
    "",
    "    -w, --workspace DIR  The folder holding the repos (default: this one)",
    "        --repo OWNER/REPO  Cover only this repo: repeatable, or one",
    "                         comma-separated list. It also names the GitHub repo",
    "                         whose origin remote names none",
    "        --project SLUG   Check that the repos are in this Carrick project.",
    "                         Repos change project on the dashboard, so a project",
    "                         they are not in stops the run",
    "        --mcp EDITOR     Add Carrick to this editor's own MCP configuration,",
    "                         a file outside this workspace: repeatable, or one",
    "                         comma-separated list. Without it, each editor whose",
    "                         command is on this machine gets the entry",
    "        --install-global Install carrick on this machine where there is none,",
    "                         so the agent hooks can run it by name after this run",
    "                         ends",
    "",
    `What init installs, the hooks included: ${DOCS_INIT_FILES}`,
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
 * one gets half the answers, and the summary line names only what is here.
 * `project_repos` is the workspace read's answer to that: the whole
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

/**
 * The one line that says what is set up: the project and the repos in it
 * from this run (carrick#1489, carrick#1512). The line says what it reports:
 * a bare "Shop (shop): api" did not (carrick#1574).
 */
export function summaryLine(project: string | null, repos: string[]): string {
  const sorted = [...repos].sort();
  const names = sorted.length > 10 ? `${sorted.slice(0, 10).join(", ")} and ${sorted.length - 10} more` : sorted.join(", ");
  const where = project === null ? "Set up with no project" : `Set up in project ${project}`;
  return repos.length === 0 ? where : `${where}: ${names}`;
}

/**
 * The installs the local read would have needed for full types, as commands.
 *
 * The scanner states them (`not_installed` on each derived repo, from its
 * preflight rule), so this only says where each runs, as a path from the
 * workspace (carrick#1489). A repo read in from the hosted index carries the
 * hosted scan's types, captured with dependencies installed, so it is not
 * named (carrick#2839).
 */
export function installCommands(plan: WorkspaceProposal, readFromHosted: string[] = []): string[] {
  const commands: string[] = [];
  const hosted = new Set(readFromHosted.map(realPath));
  for (const repo of plan.repos) {
    if (hosted.has(realPath(repo.path))) continue;
    for (const row of repo.not_installed) {
      const where = path.join(path.relative(plan.workspace, repo.path), row.directory);
      const command = where === "." ? `\`${row.command}\`` : `\`${row.command}\` in ${where}`;
      if (!commands.includes(command)) commands.push(command);
    }
  }
  return commands;
}

/**
 * The sentence that installs them, when there is anything to install. There is
 * no scan on this machine to install them for, so it ends on the read.
 */
export function installSentence(commands: string[]): string | null {
  if (commands.length === 0) return null;
  return `Dependencies are not installed, so the index here was read without their types. Run ${listed(commands, 5)}, then carrick refresh.`;
}

/**
 * The files the run creates, as one line under "Next:" (carrick#1489,
 * carrick#1512).
 *
 * Directories, because each holds several files and the line has to stay one
 * sentence; every file outside the workspace by its own path, because those
 * belong to somebody's editor. Claude Code's server list is written by its own
 * command, so it is named rather than given a path. It says "add", because
 * that is what happens: every writer here merges. And it says where: a folder
 * of repos gets the hooks and skills in the folder and again in each repo,
 * with that repo's `.git/info/exclude` (carrick#1512, option A), so `folder`
 * is the folder as the reader types it, and null for a run in one repo.
 */
export function writesLine(options: {
  workspaceFile: boolean;
  editorFiles: string[];
  claudeCode: boolean;
  folder: string | null;
  home?: string;
}): string {
  const mcp = [
    ...(options.claudeCode ? ["Claude Code"] : []),
    ...options.editorFiles.map((file) => tilde(file, options.home)),
  ];
  const where = options.folder === null ? "" : ` in ${options.folder} and in each repo`;
  const parts = [
    `Add Carrick's hooks and skills to .claude/, .agents/ and .codex/${where}`,
    options.workspaceFile ? `create .carrick/ and ${WORKSPACE_FILE}` : "create .carrick/",
    ...(mcp.length > 0 ? [`add Carrick's MCP server to ${listed(mcp, 5)}`] : []),
  ];
  return `${parts.slice(0, -1).join(", ")}, and ${parts.at(-1)}`;
}

/** The list of what the run writes, under its heading. */
export function nextBlock(lines: string[]): string {
  return listBlock("Next:", lines);
}

/**
 * The line a run inside one repo prints about the others beside it: it sets
 * up this repo, and the folder run is the one that covers them.
 */
export function thisRepoOnly(repo: string): string {
  return `Setting up ${repo} only. To set up several repos as one system, run carrick init in the folder that holds them.`;
}

/**
 * An end to the run with its own exit code, decided before anything is written.
 *
 * Thrown, every stop ends in one place, beside the cancel, which is the only
 * way to be sure no path between them reaches a write.
 */
class Stop extends Error {
  readonly code: number;
  constructor(code: number, message = "") {
    super(message);
    this.name = "Stop";
    this.code = code;
  }
}

/** The line a run that was stopped at the sign-in ends on (carrick#1338). */
export const CANCELLED = "Cancelled. Nothing was written, here or in Carrick.";

/**
 * The repos `--repo` names, out of the repos this folder holds (carrick#1338,
 * carrick#991).
 */
function namedRepos(candidates: RepoIdentity[], repos: string[], out: InitOutput): RepoIdentity[] {
  const chosen = selectRepos(candidates, repos);
  if ("problem" in chosen) throw new Error(chosen.problem);
  if (chosen.taken !== null) {
    out.done(`${chosen.taken.name} taken as the GitHub repository for ${chosen.taken.path}`);
  }
  return chosen.repos;
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
 * The editors outside the workspace that get an MCP entry: the ones `--mcp`
 * names, or else every one whose own command answers on this machine. A file
 * for an editor this machine does not run is never written unasked
 * (carrick#1365).
 */
export function chooseEditors(offered: OfferedClient[], named: string[]): string[] {
  if (offered.length === 0) return [];
  if (named.length === 0) return offered.filter((client) => client.installed).map((client) => client.name);
  const known = new Map(offered.map((client) => [client.name.toLowerCase(), client.name]));
  const chosen: string[] = [];
  for (const wanted of named) {
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

/** The GitHub repos these folders name, once each. */
function repoNames(repos: RepoIdentity[]): string[] {
  return [...new Set(repos.map((repo) => repo.name).filter((name): name is string => name !== null))];
}

/**
 * What a run inside one repo says about the folder above: the repos beside
 * this one, or a folder that is itself a repo.
 */
function aloneLines(
  current: RepoIdentity,
  parent: NonNullable<WorkspaceProposal["parent_proposal"]>,
  siblings: string[],
  out: InitOutput,
): void {
  if (siblings.length > 0) {
    out.say(thisRepoOnly(path.basename(current.path)));
  } else if (parent.repos.some((repo) => realPath(repo) === realPath(parent.directory))) {
    // Not a folder of repos but a repository this directory sits inside,
    // which is where a setup of it belongs: the one case where "run it in
    // the folder above" is still the answer.
    out.warn(`The parent folder ${parent.directory} is itself a repo. Run carrick init .. to initialise that workspace.`);
  }
}

/**
 * The repos that name no GitHub repository (carrick#991).
 *
 * A repo with no identity cannot be in a Carrick workspace, and a run that
 * dropped it in silence read as a complete one. A folder of clones can hold
 * dozens, so the list is capped and the rest is counted rather than dropped.
 */
function warnMissingIdentities(missing: RepoIdentity[], out: InitOutput): void {
  for (const repo of missing.slice(0, 10)) {
    out.warn(`${repo.path} contributes no GitHub identity: ${repo.problem}.`);
  }
  if (missing.length > 10) {
    out.warn(`${missing.length - 10} more repos here name no GitHub identity either.`);
  }
  if (missing.length > 0) {
    const one = missing.length === 1;
    // The ssh fix is said as the one it is: "the alias" named nothing for a
    // repo with no remote at all (carrick#1574).
    out.warn(
      `Carrick has nothing to connect ${one ? "it" : "them"} to. Run carrick init --repo owner/repo${one ? "" : " in each of them"}, or, for an origin on an ssh alias, give that alias a HostName github.com line in your ssh config.`,
    );
  }
}

/** What a run with no credential and no terminal to sign in from stops on. */
const LOGIN_REQUIRED = "carrick init requires a Carrick login. Run carrick login, or set CARRICK_TOKEN.";

/**
 * What init settles from the workspace read (carrick#2219).
 *
 * Nothing in it was chosen here. The repos are the ones of this folder the
 * workspace holds, each in the project the workspace has it in, and the
 * editors are the ones found on this machine.
 */
type InitPlan = {
  credential: Credential;
  /** The one workspace read this run is decided and reported from. */
  identity: ResolvedRepos;
  derived: WorkspaceProposal;
  selected: RepoIdentity[];
  names: string[];
  /** The directory names left out, for the workspace file. */
  deselected: string[];
  projects: Project[] | null;
  /** The project every covered repo is in, or null where they are in several. */
  project: string | null;
  /** The covered repos by project, as folder names, for the summary lines. */
  groups: Array<{ slug: string; repos: string[] }>;
  editors: string[];
  /** The covered repos Carrick holds no index for yet. */
  waiting: string[];
  /** The covered repos and the services the workspace read holds for them. */
  counts: { repos: number; services: number };
};

/**
 * Decide the run from the workspace read, asking nothing.
 *
 * The workspace has already answered every question a setup could ask:
 * which repos, which project, which editors. So the run reads the answers and
 * prints what it adds. It writes nothing: a stop here leaves the machine as it
 * was.
 */
async function planRun(input: {
  parsed: InitOptions;
  workspace: string;
  derived: WorkspaceProposal;
  candidates: RepoIdentity[];
  identity: ResolvedRepos;
  credential: Credential;
  out: InitOutput;
}): Promise<InitPlan> {
  const { parsed, workspace, derived, candidates, identity, credential, out } = input;
  const repos = reposUrl(identity.workspace.slug);
  const parent = derived.parent_proposal;
  if (parent !== null && candidates.length === 1) {
    aloneLines(candidates[0]!, parent, siblingRepos(derived, excludedRepos(parent.directory)), out);
  }
  // `--repo` still narrows the run, and names nothing the reader did not.
  const chosen = parsed.repos.length === 0 ? candidates : namedRepos(candidates, parsed.repos, out);
  const held = (repo: RepoIdentity): Extract<ResolvedRepos["repos"][number], { connected: true }> | null => {
    const row = identity.repos.find((entry) => entry.full_name.toLowerCase() === repo.name?.toLowerCase());
    return row?.connected === true ? row : null;
  };
  const selected = chosen.filter((repo) => held(repo) !== null);
  warnMissingIdentities(chosen.filter((repo) => repo.name === null), out);
  if (selected.length === 0) {
    out.refuse(
      `None of the repos here is in your Carrick workspace ${identity.workspace.slug}. Add them at ${repos}. If they are in another workspace, run carrick logout, then carrick init, and choose that workspace when you sign in.`,
    );
    throw new Stop(1);
  }
  const groups: InitPlan["groups"] = [];
  for (const repo of selected) {
    const slug = held(repo)!.project_slug;
    const group = groups.find((entry) => entry.slug === slug);
    if (group === undefined) groups.push({ slug, repos: [path.basename(repo.path)] });
    else group.repos.push(path.basename(repo.path));
  }
  const project = groups.length === 1 ? groups[0]!.slug : null;
  // The project is read here and changed on the dashboard, so a flag that
  // would move a repo is refused rather than half-honoured.
  if (parsed.project !== null && parsed.project !== project) {
    out.refuse(`On this workspace repos change system on the dashboard: ${repos}. Run carrick init without --project.`);
    throw new Stop(1);
  }
  const absent = repoNames(chosen.filter((repo) => held(repo) === null));
  if (absent.length > 0) {
    const one = absent.length === 1;
    out.warn(
      `${listed(absent, 10)} ${one ? "is" : "are"} not in your Carrick workspace ${identity.workspace.slug}, so this run leaves ${one ? "it" : "them"} out. Add repos at ${repos}, then run carrick init again.`,
    );
  }
  const names = repoNames(selected);
  const projects = await listProjects(credential.token);
  for (const missing of derived.missing) out.warn(`${WORKSPACE_FILE} lists ${missing}, which is not a directory on this machine.`);
  const offered = offeredFileClients();
  const editors = chooseEditors(offered, parsed.editors);
  const kept = new Set(selected.map((repo) => repo.path));
  const deselected = candidates.filter((repo) => !kept.has(repo.path)).map((repo) => path.basename(repo.path));
  // Every file is named before it is written.
  out.say(
    nextBlock([
      writesLine({
        workspaceFile: deselected.length > 0,
        editorFiles: offered.filter((client) => editors.includes(client.name)).map((client) => client.file),
        claudeCode: claudeCodeFound(),
        folder: selected.some((repo) => realPath(repo.path) !== realPath(workspace)) ? tilde(realPath(workspace)) : null,
      }),
    ]),
  );
  return {
    credential,
    identity,
    derived: keepRepos(derived, selected.map((repo) => repo.path)),
    selected,
    names,
    deselected,
    projects,
    project,
    groups,
    editors,
    waiting: repoNames(selected.filter((repo) => held(repo)!.services.length === 0)),
    counts: {
      repos: selected.length,
      services: selected.reduce((sum, repo) => sum + held(repo)!.services.length, 0),
    },
  };
}

export async function init(argv: string[]): Promise<number> {
  return initWith(argv, createOutput(), process.stdin.isTTY === true);
}

/**
 * `init` with the terminal stated rather than sniffed, so a test can be the
 * terminal. The terminal decides only whether init can sign in from here and
 * how a step draws: nothing is asked after sign-in.
 */
export async function initWith(argv: string[], out: InitOutput, interactive: boolean): Promise<number> {
  const parsed = parseArgs(argv);
  if (typeof parsed === "string") {
    process.stdout.write(`${parsed}\n`);
    return parsed.startsWith("carrick init") ? 0 : 2;
  }
  // Where the run was started, and where it sets up: the folder above, when
  // an earlier run there covers this repo (carrick#1512).
  const startedIn = parsed.workspace;
  let workspace = parsed.workspace;

  if (!fs.existsSync(workspace)) {
    process.stderr.write(`carrick init: ${workspace} is not a directory on this machine\n`);
    return 1;
  }
  const above = initFolderAbove(workspace);
  if (above !== null) {
    out.say(`${path.basename(realPath(workspace))} is set up with the repos in ${tilde(above)}, so this run sets up ${tilde(above)}.`);
    workspace = above;
  }
  // How to run this again, naming the folder where the run set up a folder
  // other than the one it started in.
  const again = (flags = ""): string =>
    realPath(workspace) === realPath(startedIn)
      ? `run carrick init${flags} again`
      : `run carrick init${flags} in ${tilde(workspace)} again`;

  let plan: InitPlan;
  // Puts the workspace file back as it was found, for a run that stops before
  // it writes anything.
  let restoreSelection: (() => void) | null = null;
  try {
    // What a previous run wrote down, before anything is derived from it: the
    // scanner has already dropped an excluded repo by the time the derivation
    // arrives, so this is the only place that can tell a reader why the repo
    // they just named is not here (carrick#1344).
    const excluded = excludedRepos(workspace);
    const asked = namedButExcluded(workspace, excluded, parsed.repos);
    if (asked.length > 0) {
      throw new Error(
        `${WORKSPACE_FILE} leaves ${asked.join(" and ")} out of this workspace, so --repo cannot cover ${asked.length === 1 ? "it" : "them"}. Take ${asked.length === 1 ? "that name" : "those names"} out of the exclude list there and run carrick init again.`,
      );
    }
    const leftOut = (names: string[]): void => {
      if (names.length === 0) return;
      out.say(
        `${WORKSPACE_FILE} leaves ${names.join(", ")} out of this workspace, so nothing below covers ${names.length === 1 ? "it" : "them"}.`,
      );
    };
    // The derivation and the identities are local reads.
    let derived: WorkspaceProposal;
    try {
      derived = deriveWorkspace(workspace);
    } catch (error) {
      leftOut(excluded);
      throw error;
    }
    let candidates = derived.repos.map((repo) => repoIdentity(repo.path));
    // The read names the repos of this folder, or the `--repo` values alone: a
    // repo the reader left out is named in no request (carrick#1338).
    const here = parsed.repos.length > 0 ? parsed.repos : repoNames(candidates);
    // Nothing here can be in a Carrick workspace, so there is nothing to sign
    // in for and nothing to set up.
    if (here.length === 0) {
      leftOut(excluded);
      warnMissingIdentities(candidates.filter((repo) => repo.name === null), out);
      throw new Stop(1);
    }
    let saved = readCredential();
    if (saved === null) {
      if (!interactive) {
        leftOut(excluded);
        throw new Error(LOGIN_REQUIRED);
      }
      saved = await signIn(out.say);
    }
    // One read decides the run: which of these repos the workspace holds, in
    // which project, with which index. A read that fails is said in its own
    // words, and nothing is written.
    let identity = await resolveRepos(saved.token, here);
    // A repo that has joined the workspace since the last run. The names init
    // recorded in the workspace file are a result, not a choice, so they are
    // taken back out and the folder is read whole again. A name somebody wrote
    // into `exclude` by hand stays, and is named in no request. `--repo` is a
    // choice, and leaves the record alone.
    if (parsed.repos.length === 0) {
      const before = readWorkspaceFile(workspace);
      if (before !== null && removeSelection(workspace) !== null) {
        restoreSelection = () => fs.writeFileSync(path.join(workspace, WORKSPACE_FILE), before);
        derived = deriveWorkspace(workspace);
        candidates = derived.repos.map((repo) => repoIdentity(repo.path));
        identity = await resolveRepos(saved.token, repoNames(candidates));
      }
    }
    leftOut(excludedRepos(workspace));
    plan = await planRun({ parsed, workspace, derived, candidates, identity, credential: saved, out });
  } catch (error) {
    restoreSelection?.();
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

  // From here the run writes: the selection, the hooks, the skills and the
  // MCP entries.
  const { derived, credential, names, projects, project, deselected, editors, selected } = plan;
  const label = (slug: string): string => projectLabel(slug, projects);
  // What the reader still has to do, said once at the end rather than between
  // the steps (carrick#1489).
  const todo: string[] = [];
  // Whether the closing block carries the Codex trust step: only when this run
  // wrote the Codex hooks for the first time (carrick#3286).
  let codexTrust = false;
  try {
    ensureIndexDir(derived.workspace);
    // The selection, in the file every later command reads. Without it the
    // choice lasted one run: `carrick refresh` walks the folder, the editor
    // hook answers for a file inside a repo nobody covers, and the next `init`
    // reads it again (carrick#1344).
    writeSelection(derived.workspace, deselected);
  } catch (error) {
    process.stderr.write(`carrick init: ${(error as Error).message}\n`);
    return 1;
  }
  // Before the hook commands are decided, because what they say depends on the
  // answer: with a global, they are the bare `carrick`, which every shell and
  // every agent on this machine resolves; without one, they name this install,
  // and an npx cache is cleared (carrick#1372). A machine that already has one
  // was brought level before this command started. Nothing is asked: the line
  // names the command, and `--install-global` runs it.
  try {
    await offerGlobalInstall({
      assumeYes: true,
      install: parsed.installGlobal,
      confirm: (question) => out.confirm(question),
      say: (line) => out.warn(line),
    });
  } catch (error) {
    if (!(error instanceof PromptCancelled)) {
      out.warn(`The global install was not offered: ${(error as Error).message}`);
    }
  }
  const command = hookCommand({ onPath: carrickOnPath });
  const settingsName = path.join(".claude", command.bare ? "settings.json" : "settings.local.json");
  const settingsFile = path.join(workspace, settingsName);
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
    out.refuse(
      `Could not configure Carrick hooks: ${(error as Error).message}. Fix ${settingsName} and ${again()}.`,
    );
  }
  // The same hooks for Codex, in the file Codex reads project hooks
  // from. Written for both hosts for the same reason the task skills are
  // (carrick#1335): which agent opens this workspace is not a thing init can
  // know, and a hook file for a host nobody runs costs nothing.
  try {
    // Said once, last, in the closing block.
    if (writeCodexHooks(workspace, command.command) === "written") codexTrust = true;
  } catch (error) {
    out.refuse(
      `Could not configure the Codex hooks: ${(error as Error).message}. Fix ${CODEX_HOOKS_FILE} and ${again()}.`,
    );
  }

  // The task skills, beside the hooks and independent of them: a settings file
  // somebody hand-edited into invalid JSON is no reason to withhold the bodies
  // an agent reads. A file this package did not write, or one somebody has
  // since changed, is left alone and named.
  try {
    todo.push(...taskSkillWarnings(writeTaskSkills(workspace, { slug: project })));
    // Which build wrote them, so a hook running an older `carrick` than this
    // one says so instead of answering from files it does not match
    // (carrick#1372). It is also the record a run inside one repo of this
    // folder finds (`initFolderAbove`).
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

  // The check block in AGENTS.md (carrick#2828): only our marked section is
  // written, and every other line of the file is left as it is.
  try {
    writeAgentsSection(workspace, { slug: project }, plan.counts);
  } catch (error) {
    out.refuse(`Could not write the Carrick check section in ${AGENTS_FILE}: ${(error as Error).message}`);
  }

  // The same hooks and skills inside each repo of a folder, because an agent
  // started inside one of them reads neither host's files from the folder
  // above (carrick#1512, option A as ruled). Kept out of git through each
  // repo's own exclude file; a repo that is the workspace already has them.
  for (const repo of selected) {
    if (realPath(repo.path) === realPath(workspace)) continue;
    try {
      writeRepoCopy(repo.path, command.command, { slug: project });
    } catch (error) {
      out.refuse(`Could not add Carrick's hooks and skills to ${path.basename(repo.path)}: ${(error as Error).message}.`);
    }
  }

  // The MCP connection, for work that crosses repos this machine does not
  // hold, configured for every client this machine has (carrick#955). Nothing
  // here about restarting Claude Code: the closing block opens a new agent
  // session, which loads the server it has just been given (carrick#1512).
  const mcp = connectMcpClients(editors);
  for (const outcome of mcp.filter((entry) => entry.state === "failed")) {
    todo.push(`MCP not configured for ${outcome.client}: ${outcome.detail}`);
  }
  // Said as what is missing, not as "no agent client": the hooks above were
  // written for two of them (carrick#1489).
  if (!mcp.some((outcome) => outcome.client === "Claude Code")) {
    todo.push(`No claude command on this machine, so Claude Code has no Carrick MCP server. Once it is installed: ${mcpLine()}`);
  }

  if (plan.identity.allowance_sentence) out.say(plan.identity.allowance_sentence);
  // The rest of the project, which this machine does not hold. Carrick
  // answers across every repo in a project, so a folder holding half of one
  // is a partial index and nothing here would otherwise say so
  // (carrick#993 row 18). "This machine" is every repo in the folder that
  // holds this run's repos, set up here or not: a repo in the folder beside
  // this one is not missing (carrick#1512). Only read where the run's own
  // repos leave something unaccounted for, because each is a git call.
  if (absentRepos(plan.identity, names, project, label).length > 0) {
    const folder = selected.some((repo) => realPath(repo.path) === realPath(workspace))
      ? path.dirname(workspace)
      : workspace;
    const here = repoRoots(folder)
      .map((root) => repoIdentity(root).name)
      .filter((name): name is string => name !== null);
    todo.push(...absentRepos(plan.identity, [...names, ...here], project, label));
  }

  // What is set up, one line per project the workspace has these repos in.
  for (const group of plan.groups) out.done(summaryLine(label(group.slug), group.repos));

  // The read always runs, index or not: it is what leaves `.carrick/index.json`
  // here, and the session-start hook reads a later hosted index in only onto a
  // machine that has one (carrick#2219). It reports its own outcome, so it is
  // one line in a terminal as well as in a pipe (carrick#1032), and what it
  // says while it runs is the scanner's own count (carrick#1365). The repo
  // list goes in because the step decides for itself whether to re-read this
  // tree, and a repo the index does not cover is the one way that decision
  // cannot be read off a drift count (carrick#1373).
  const startedAt = Date.now();
  const read = await out.step(
    STEP_LABEL,
    (progress) =>
      downloadHostedIndex(
        derived.workspace,
        nativeRunner(out.quiet, progress),
        derived.repos.map((repo) => repo.path),
        progress,
      ),
    (outcome) => hostedReport(outcome, (Date.now() - startedAt) / 1000),
  );
  const downloaded = read.kind === "downloaded";
  const readFromHosted = read.kind === "downloaded" ? read.readFromHosted : [];
  for (const line of todo) out.warn(line);
  // There is no workflow to write, no config to seed and no scan to run from
  // here, so the run closes on what this machine holds and on what it is
  // still waiting for (carrick#2219, carrick#3361).
  if (plan.waiting.length > 0) {
    const one = plan.waiting.length === 1;
    out.warn(
      `Carrick has no index for ${listed(plan.waiting, 10)} yet. It builds ${one ? "it" : "them"} in the cloud, and ${reposUrl(plan.identity.workspace.slug)} shows where ${one ? "it is" : "they are"}. Run carrick refresh here once ${one ? "it is" : "they are"} ready.`,
    );
  }
  const install = installSentence(installCommands(derived, readFromHosted));
  if (install !== null) out.warn(install);
  if (downloaded) out.done(READY_SENTENCE);
  out.note(CLOUD_NEXT_TITLE, cloudNextLines(codexTrust, plan.waiting.length > 0 || !downloaded));
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
