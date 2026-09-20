// Authenticated setup; Rust owns the workspace and service proposal.
//
// What this prints is one line per thing that happened (carrick#1026). The
// reasoning behind each step — how the hooks deliver, which editor extension
// to install, why the scan is left to CI, what a hand-written carrick.json
// looks like — is the quickstart's, at `output.ts`'s `DOCS`, and this command
// links to it rather than repeating it. The record of the run stays here,
// because nothing else can state it: this login, this project, these repos,
// this many packages, this index.

import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { readCredential, type Credential } from "../auth/credentials.ts";
import { signIn } from "../auth/run.ts";
import { resolveRepos, type ResolvedRepos } from "../auth/read.ts";
import {
  deriveWorkspace,
  selectedProposal,
  selectRepos,
  writeProposal,
  PROPOSAL_FILE,
  repoIdentity,
  type DerivedWorkspace,
  type RepoIdentity,
  type WorkspaceProposal,
} from "./repos.ts";
import { connectRepos, reposAreInProject, projectAssignments } from "./connect.ts";
import { downloadHostedIndex, hostedReport, nativeRunner } from "./hosted.ts";
import {
  createProject,
  listProjects,
  planProject,
  projectLabel,
  projectStep,
  SLUG,
  type Project,
  type ProjectChoice,
} from "./projects.ts";
import { connectMcpClients, mcpLine, type McpOutcome } from "./mcp.ts";
import { hookCommand, mergeCarrickHooks, removeCarrickHooks } from "./settings.ts";
import { CODEX_HOOKS_FILE, writeCodexHooks } from "./codex.ts";
import { ignoredSkillRoots, taskSkillLines, writeTaskSkills } from "./task-skills.ts";
import { writeIfChanged } from "./files.ts";
import { createOutput, DOCS, PromptCancelled, type Choice, type InitOutput } from "./output.ts";
import { renderTemplate } from "../templates.ts";

/**
 * The one sentence the run ends on.
 *
 * The scaffold tool carries the instructions — which files to create, how to
 * seed `carrick.json` from the proposal, whether to run the scan at all
 * (carrick-cloud#832) — so the terminal names the tool and stops. The copy
 * that used to stand here was a second statement of the same instructions, and
 * two copies of a sequence drift.
 *
 * The argument is named because the tool's branch turns on it and turns soft
 * without it: `repo` is what resolves the hosted rows, and with no `repo` the
 * response keeps its default ending and tells the agent to run the scan — on a
 * repo CI already indexes, which is the row the whole workspace reads
 * (carrick-cloud `src/tools/scaffold.ts`, cloud#805 item 1).
 */
export const SCAFFOLD_SENTENCE =
  "Run the carrick scaffold tool for this repo, passing its owner/repo as `repo`, and follow what it returns.";

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
  /** Answer yes to the proposal rather than asking. Never grants a move. */
  assumeYes: boolean;
  /** Take the selected repos out of whatever project they are in now. */
  allowMove: boolean;
};

const OWNER_REPO = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;

export function parseArgs(argv: string[], cwd = process.cwd()): InitOptions | string {
  const options: InitOptions = { workspace: cwd, project: null, repos: [], assumeYes: false, allowMove: false };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    switch (argument) {
      case "--yes":
      case "-y":
        options.assumeYes = true;
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
    "carrick init [DIRECTORY] [--repo OWNER/REPO]... [--project SLUG] [--allow-move]",
    "",
    "Sign in (here, or beforehand with carrick login), then set up a repository",
    "or a folder of repos: which repos this install covers, the project, the repo",
    "connection, the agent hooks, the MCP connection, and the service proposal",
    "your agent turns into carrick.json. It writes nothing into the repository but",
    "the ignored .carrick directory, the hook settings, and the four task skills",
    "your agent loads. It runs no analysis: where Carrick already holds an index",
    "for these repos, it reads that index into .carrick so this machine can answer",
    "from it. Nothing is written, here or in Carrick, until you accept what it",
    "proposes.",
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
    "    -y, --yes            Take the proposal as printed",
    "",
    `The editor extension, the hooks, CI and a carrick.json written by hand: ${DOCS}`,
  ].join("\n");
}

/** Whether a command answers on this machine, for a line we should not print. */
function onPath(command: string): boolean {
  const probe = spawnSync(process.platform === "win32" ? "where" : "which", [command], {
    stdio: "ignore",
  });
  return probe.status === 0;
}

/**
 * The repos a project holds that this folder does not, one line per project.
 *
 * Carrick answers across every repo in a project, so a machine holding half of
 * one gets half the answers, and the connection line above names only what is
 * here. `project_repos` is the workspace read's answer to that: the whole
 * membership of every project the requested repos are in (carrick#993 row 18).
 * Capped and counted like the identity lines, because a project can hold two
 * hundred.
 */
export function absentRepos(
  identity: ResolvedRepos,
  names: string[],
  project: string | null,
  label: (slug: string) => string = (slug) => slug,
): string[] {
  const onDisk = new Set(names.map((name) => name.toLowerCase()));
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

/** The repos this run connected, as one line however many there are. */
export function connectedLine(names: string[]): string {
  if (names.length === 1) return `Repo ${names[0]} connected`;
  const shown = names.length > 3 ? `${names.slice(0, 3).join(", ")} and ${names.length - 3} more` : names.join(", ");
  return `${names.length} repos connected: ${shown}`;
}

/**
 * What the derive found, in the words the workspace stated it in.
 *
 * The manifest kind is the scanner's own `reason` for each repo — "npm
 * workspaces", "pnpm workspaces", "Deno manifests", "carrick.json", "single
 * repository", or a `+` join of them (`src/service_derivation.rs`) — and it is
 * named only where every repo here agrees on one, because a mixed workspace
 * has no one word for what was found.
 */
export function packagesFound(plan: WorkspaceProposal): string {
  const count = plan.repos.reduce((total, repo) => total + repo.services.length, 0);
  const reasons = plan.repos.map((repo) => repo.reason);
  const every = (word: string): boolean => reasons.length > 0 && reasons.every((reason) => reason.includes(word));
  const kind = every("Deno") ? "Deno " : every("pnpm") ? "pnpm " : every("npm") ? "npm " : "";
  return `${count} ${kind}package${count === 1 ? "" : "s"}`;
}

/** The line that states it, and names where the proposal landed. */
export function packagesLine(plan: WorkspaceProposal): string {
  return `${packagesFound(plan)} found, proposal in ${PROPOSAL_FILE}`;
}

/**
 * The one line for the hooks and the MCP connection.
 *
 * A client is named only where this run changed something for it: an "already
 * connected" line is a line about nothing, and a first run prints four of them
 * (carrick#1026). The hooks are always this repo's, so they are always the
 * subject.
 */
export function configuredLine(mcp: McpOutcome[]): string {
  // `unstamped` is not `written`: that run changed nothing for Claude Code,
  // and the line must not say it did.
  const claude = mcp.some((outcome) => outcome.client === "Claude Code" && outcome.state === "written");
  return claude
    ? "Claude Code hooks and MCP configured (restart the client)"
    : "Claude Code hooks configured";
}

/**
 * One line per other client this run wrote a file for, naming that file.
 *
 * The path is printed because this command guessed it: a client detected by its
 * own data directory gets an entry written into its own config file, and a
 * wrong guess has to be one visible line and one entry to delete (`mcp.ts`).
 * A client left exactly as it was gets no line at all (carrick#1026).
 */
export function mcpClientLines(mcp: McpOutcome[]): string[] {
  return mcp
    .filter((outcome) => outcome.state === "written" && outcome.client !== "Claude Code")
    .map((outcome) => `MCP added for ${outcome.client}: ${outcome.detail}`);
}

/**
 * One warning per client connected before the install id existed.
 *
 * Not a failure — the client works, and this run deliberately changed nothing
 * on its entry — so the line is the client, what is missing, and the commands
 * that put it there, which are the owner's to run (`mcp.ts`).
 */
export function mcpUnstampedLines(mcp: McpOutcome[]): string[] {
  return mcp
    .filter((outcome) => outcome.state === "unstamped")
    .map((outcome) => `${outcome.client}: ${outcome.detail}`);
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

/** A repo as the picker lists it: what it is called, and what is in it. */
export function repoChoices(plan: WorkspaceProposal, candidates: RepoIdentity[]): Choice[] {
  return candidates.map((repo) => {
    const found = plan.repos.find((entry) => entry.path === repo.path);
    const packages = found?.services.length ?? 0;
    const hint = [
      `${packages} package${packages === 1 ? "" : "s"}`,
      ...(repo.name === null ? ["no GitHub identity"] : []),
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
  options: { repos: string[]; assumeYes: boolean; interactive: boolean },
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
  const kept = new Set(
    await out.choose("Which repos does this install cover?", repoChoices(plan, candidates)),
  );
  return candidates.filter((repo) => kept.has(repo.path));
}

/** The repos this install covers, one line each, for the proposal. */
export function coverageLines(plan: WorkspaceProposal, selected: RepoIdentity[]): string[] {
  return selected.slice(0, 10).map((repo) => {
    const found = plan.repos.find((entry) => entry.path === repo.path);
    const packages = found?.services.length ?? 0;
    return `  ${repo.name ?? path.basename(repo.path)}  ${packages} package${packages === 1 ? "" : "s"}`;
  });
}

/**
 * What the server would be asked to change, said before it is asked.
 *
 * A move is named with the project it comes OUT of, display name and slug
 * both, because that is the project whose agents stop seeing this repo and
 * the one a reader has to find again to undo it (carrick#1338).
 */
export function moveLines(
  moving: string[],
  from: (name: string) => string,
  target: string,
  label: (slug: string) => string,
): string[] {
  return moving.map((name) => `  ${name} will move from ${label(from(name))} to ${label(target)}`);
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
  const { workspace } = parsed;

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
  try {
    // The derivation and the identities are local reads: they name the choice,
    // and a run nobody has chosen in yet must not have signed anything in.
    derived = deriveWorkspace(workspace);
    const candidates = derived.plan.repos.map((repo) => repoIdentity(repo.path));
    const selected = await chooseRepos(
      derived.plan,
      candidates,
      { repos: parsed.repos, assumeYes: parsed.assumeYes, interactive },
      out,
    );
    if (selected.length === 0) throw new Stop(0);
    derived = selectedProposal(derived, selected.map((repo) => repo.path));
    const dropped = candidates.length - selected.length;
    if (dropped > 0) {
      out.done(
        `${selected.length} of ${candidates.length} repos covered; ${dropped} left out of the proposal, the project and the connection`,
      );
    }
    // A machine that has never signed in signs in here rather than being told
    // to run another command: `carrick login` is the same browser round trip,
    // and refusing to take it was the first thing a new install did
    // (carrick#955). Without a terminal there is no browser to hand it to.
    const saved: Credential | null = readCredential();
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
    initial = await resolveRepos(credential.token, names);
    // Read once, for the names: every project this run prints is printed as
    // the dashboard shows it, with the slug beside it (carrick#1338). A
    // workspace whose API has no such action answers null and the lines carry
    // slugs alone, as they did.
    projects = names.length > 0 ? await listProjects(credential.token) : null;
    // The project half of the browser round trip, where this API can do it
    // from here. Nothing is created or moved in it: it settles what this run
    // would do, and the proposal below is where that is accepted. Without
    // --project the step reads the assignment the repos already have and
    // offers the list, rather than doing nothing at all (carrick#987).
    const prompts = { say: out.say, ask: out.ask, confirm: out.confirm, interactive, assumeYes: parsed.assumeYes };
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
    if (plan.parent_proposal) {
      const parent = plan.parent_proposal;
      const folders = parent.repos.map((repo) => path.basename(repo));
      const shown = folders.length > 3 ? `${folders.slice(0, 3).join(", ")} and ${folders.length - 3} more` : folders.join(", ");
      out.warn(`The parent folder ${parent.directory} holds ${parent.repos.length} ${parent.repos.length === 1 ? "repo" : "repos"}: ${shown}. Run carrick init .. to initialise that workspace.`);
    }
    // The scanner's own warnings about what it proposed. Capped: the proposal
    // file carries every one of them, and it is named on the line above.
    const warnings = plan.repos.flatMap((repo) => repo.warnings);
    for (const warning of warnings.slice(0, 3)) out.warn(warning);
    if (warnings.length > 3) out.warn(`${warnings.length - 3} more notes on the proposal are in ${PROPOSAL_FILE}.`);
    for (const missing of plan.missing) out.warn(`Missing workspace override: ${missing}`);
    if (selected.length > 1) {
      out.say("This install covers:");
      for (const line of coverageLines(plan, selected)) out.say(line);
      if (selected.length > 10) out.say(`  and ${selected.length - 10} more`);
    }
    if (project !== null) {
      for (const line of moveLines(moving, (name) => assigned.get(name) ?? "", project, label)) out.say(line);
      if (joining.length > 0) {
        out.say(`  ${joining.join(", ")} will be placed in project ${label(project)} once the browser connects ${joining.length === 1 ? "it" : "them"}`);
      }
      if (decision.create) out.say(`  Project "${project}" will be created in this workspace`);
    }
    const subject = packagesFound(plan);
    if (!parsed.assumeYes) {
      if (!interactive) {
        throw new Stop(
          1,
          `use --yes to accept this proposal without a terminal${moving.length > 0 && !parsed.allowMove ? ", and --allow-move to accept the move above" : ""}.`,
        );
      }
      // The count is in the question, because the listing that used to stand
      // above it is gone: this is where a reader decides (carrick#1026).
      if (!(await out.confirm(`Write the proposal for ${subject} and configure hooks?`))) {
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
  try {
    let projectExists = decision.exists;
    if (decision.create && project !== null) {
      const outcome = await createProject(credential.token, project);
      if (outcome.kind === "created") {
        out.say(`Created project "${project}".`);
        projectExists = true;
      } else if (outcome.kind === "refused") {
        out.say(`Carrick did not create "${project}": ${outcome.message}`);
      }
    }
    const identity = await connectRepos(credential.token, names, initial, {
      interactive,
      project: project ?? undefined,
      projectExists,
      movable,
      label,
      say: out.say,
    });
    // Who this machine is, and where its answers come from. The workspace slug
    // is the login: nothing on the credential or in the workspace read names a
    // user (carrick#1026).
    out.done(
      project === null
        ? `Signed in as ${identity.workspace.slug}`
        : `Signed in as ${identity.workspace.slug} · project ${label(project)}`,
    );
    if (project !== null && !reposAreInProject(identity, names, project)) {
      // An unverified project does not end the run, whether it was named on
      // the command line or picked here. Stopping cost someone their hooks and
      // their proposal for a browser step they could only take afterwards, and
      // the documented command then needed two runs (carrick#993 row 8).
      out.warn(
        `Finish the browser steps above to put these repos in ${label(project)}, then run carrick init --project ${project} again to verify.`,
      );
    }
    if (identity.allowance_sentence) out.say(identity.allowance_sentence);
    const connected: string[] = [];
    for (const repo of identity.repos) {
      if (!repo.connected) out.warn(`${repo.full_name} is not connected to this Carrick workspace.`);
      else {
        connected.push(repo.full_name);
        if (repo.services.length > 0) hostedIndex.push(repo.full_name);
      }
    }
    if (connected.length > 0) out.done(connectedLine(connected));
    // The rest of the project, which this machine does not hold. Carrick
    // answers across every repo in a project, so a folder holding half of one
    // is a partial index and nothing here would otherwise say so
    // (carrick#993 row 18).
    for (const line of absentRepos(identity, names, project, label)) out.warn(line);
  } catch (error) {
    process.stderr.write(`carrick init: ${(error as Error).message}\n`);
    return 1;
  }
  try {
    // The proposal is a seed for an agent, not a config: nothing derived
    // without a model is written into the repository, because the first scan
    // is the paid one and it has to run against a config someone has read
    // (carrick-cloud#799).
    writeProposal(plan.workspace, derived);
    out.done(packagesLine(plan));
  } catch (error) {
    process.stderr.write(`carrick init: ${(error as Error).message}\n`);
    return 1;
  }

  // The agent hooks, merged by command: this file may already hold a user's
  // own hooks, or the hook pack the hosted index installs. And the MCP
  // connection, for work that crosses repos this machine does not hold,
  // configured for every client this machine has rather than printed for one
  // of them (carrick#955).
  const command = hookCommand({ onPath });
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
  try {
    if (writeCodexHooks(workspace, command.command) === "written") {
      out.done(
        `Codex hook entries written to ${CODEX_HOOKS_FILE}. Codex asks you to trust them the next time it starts; until you do, they do not run.`,
      );
    }
  } catch (error) {
    out.refuse(
      `Could not configure the Codex hooks: ${(error as Error).message}. Fix ${CODEX_HOOKS_FILE} and run carrick init again.`,
    );
  }

  // The task skills, beside the hooks and independent of them: a settings file
  // somebody hand-edited into invalid JSON is no reason to withhold the bodies
  // an agent reads. A file this package did not write, or one somebody has
  // since changed, is left alone and named.
  try {
    const { done, warn } = taskSkillLines(writeTaskSkills(workspace, { slug: projectSlug }));
    for (const line of done) out.done(line);
    for (const line of warn) out.warn(line);
    const ignored = ignoredSkillRoots(workspace);
    if (ignored.length > 0) {
      const one = ignored.length === 1;
      out.warn(
        `${ignored.join(" and ")} ${one ? "is" : "are"} git-ignored here, so these skills stay on this machine. Track ${one ? "it" : "them"} to give the rest of the team the same answers.`,
      );
    }
  } catch (error) {
    out.refuse(`Could not write the task skills: ${(error as Error).message}`);
  }

  const mcp = connectMcpClients();
  if (hooksWritten) out.done(configuredLine(mcp));
  for (const line of mcpClientLines(mcp)) out.done(line);
  if (!command.bare) {
    out.warn(
      `\`carrick\` is not on PATH, so the hooks in ${settingsName} name this install. Run \`npm install -g carrick\` and carrick init again for the short command.`,
    );
  }
  for (const outcome of mcp.filter((entry) => entry.state === "failed")) {
    out.warn(`MCP not configured for ${outcome.client}: ${outcome.detail}`);
  }
  // Connected, and older than the install id: nothing was changed on that
  // entry and nothing here failed (carrick-cloud#890, `mcp.ts`).
  for (const line of mcpUnstampedLines(mcp)) out.warn(line);
  if (mcp.length === 0) out.warn(`No agent client found on this machine. In Claude Code: ${mcpLine()}`);

  // No paid scan ran here, and that is the point (carrick-cloud#799): the one
  // scan runs against a config someone has read. Where CI has already built an
  // index, this run reads it onto the machine instead — the hosted index is
  // the whole workspace's row, and `carrick index` from a laptop on a branch
  // replaces it for everyone who queries it (carrick#993 row 2, carrick#1020).
  if (hostedIndex.length > 0) {
    // The step reports its own outcome, so the read is one line in a terminal
    // as well as in a pipe: a spinner that stopped with its label spent a
    // second line on the same event (carrick#1032).
    await out.step(
      "Reading the hosted index into .carrick/",
      () => downloadHostedIndex(plan.workspace, nativeRunner(out.quiet)),
      hostedReport,
    );
  } else {
    out.done("No index yet: your agent runs the one scan");
  }
  out.note("Next: paste this to your agent", [SCAFFOLD_SENTENCE]);
  out.say(`Docs: ${out.accent(DOCS)}`);
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
