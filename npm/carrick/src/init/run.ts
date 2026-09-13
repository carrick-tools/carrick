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
  writeProposal,
  PROPOSAL_FILE,
  repoIdentity,
  type WorkspaceProposal,
} from "./repos.ts";
import { connectRepos, reposAreInProject, projectAssignments } from "./connect.ts";
import { downloadHostedIndex, hostedReport, nativeRunner } from "./hosted.ts";
import { ensureProject, projectStep, SLUG } from "./projects.ts";
import { connectMcpClients, MCP_LINE, type McpOutcome } from "./mcp.ts";
import { hookCommand, mergeCarrickHooks } from "./settings.ts";
import { createOutput, DOCS, type InitOutput } from "./output.ts";
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
  /** Require every proposed GitHub repo to belong to this project. */
  project: string | null;
  /** The `owner/repo` to use when the origin remote names none (carrick#991). */
  repo: string | null;
  /** Answer yes to the repo list rather than asking. */
  assumeYes: boolean;
};

const OWNER_REPO = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;

export function parseArgs(argv: string[], cwd = process.cwd()): InitOptions | string {
  const options: InitOptions = { workspace: cwd, project: null, repo: null, assumeYes: false };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    switch (argument) {
      case "--yes":
      case "-y":
        options.assumeYes = true;
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
        if (!OWNER_REPO.test(value)) return `invalid repo "${value}": use owner/repo as GitHub spells it`;
        options.repo = value;
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
    "carrick init [DIRECTORY] [--project SLUG] [--repo OWNER/REPO]",
    "",
    "Sign in (here, or beforehand with carrick login), then set up a repository",
    "or a folder of repos: the project, the repo connection, the agent hooks,",
    "the MCP connection, and the service proposal your agent turns into",
    "carrick.json. It writes nothing into the repository but the ignored",
    ".carrick directory and the hook settings, and it runs no paid scan: where",
    "Carrick already holds an index for these repos, it reads that index into",
    ".carrick so this machine can answer from it.",
    "",
    "    -w, --workspace DIR  The folder holding the repos (default: this one)",
    "        --project SLUG   Require these repos in this Carrick project",
    "        --repo OWNER/REPO  Name the GitHub repo whose origin remote names none",
    "    -y, --yes            Take the repo list as proposed",
    "",
    `The editor extension, the hooks, CI and a carrick.json written by hand: ${DOCS}`,
  ].join("\n");
}

function writeIfChanged(target: string, body: string): "written" | "unchanged" {
  const existing = fs.existsSync(target) ? fs.readFileSync(target, "utf8") : null;
  if (existing === body) return "unchanged";
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.writeFileSync(target, body);
  return "written";
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
      sole || entry.project_slug === project ? "this project" : `project "${entry.project_slug}"`;
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

export async function init(argv: string[]): Promise<number> {
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

  const interactive = process.stdin.isTTY === true;
  const out: InitOutput = createOutput();

  // Authentication and all derivation validation precede local writes.
  let derived: ReturnType<typeof deriveWorkspace>;
  // The repos the workspace read found an index for: where CI has already built
  // one, this run reads it rather than ordering a scan (carrick#993 row 2).
  const hostedIndex: string[] = [];
  try {
    // A machine that has never signed in signs in here rather than being told
    // to run another command: `carrick login` is the same browser round trip,
    // and refusing to take it was the first thing a new install did
    // (carrick#955). Without a terminal there is no browser to hand it to.
    let credential: Credential | null = readCredential();
    if (!credential) {
      if (!interactive) {
        throw new Error("carrick init requires a Carrick login. Run carrick login, or set CARRICK_TOKEN.");
      }
      credential = await signIn(out.say);
    }
    derived = deriveWorkspace(workspace);
    const derivedIdentities = derived.plan.repos.map((repo) => repoIdentity(repo.path));
    // `--repo` names what a remote could not: an ssh alias ssh itself cannot
    // resolve, a mirror, a clone with no origin. It names one repository, so
    // it is taken only when exactly one repo here is missing an identity.
    const unnamed = derivedIdentities.filter((repo) => repo.name === null);
    let taken: string | null = null;
    if (parsed.repo !== null) {
      if (unnamed.length > 1) {
        throw new Error(
          `--repo names one repository, but ${unnamed.length} repos here have no GitHub identity: ${unnamed.map((repo) => repo.path).join(", ")}. Run carrick init --repo in each of them, or fix their origin remotes.`,
        );
      }
      if (unnamed.length === 0) {
        out.warn(`--repo ${parsed.repo} was not needed: every repo here names its own GitHub repository.`);
      } else {
        taken = unnamed[0]!.path;
        out.done(`${parsed.repo} taken as the GitHub repository for ${taken}`);
      }
    }
    const repoIdentities = derivedIdentities.map((repo) =>
      repo.path === taken ? { ...repo, name: parsed.repo, problem: null } : repo,
    );
    const names = [...new Set(repoIdentities.map((repo) => repo.name).filter((name): name is string => name !== null))];
    if (names.length > 200) throw new Error("This workspace has more than 200 GitHub repos. Initialise smaller workspace groups.");
    const missingIdentities = repoIdentities.filter((repo) => repo.name === null);
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
    const initial = await resolveRepos(credential.token, names);
    // The project half of the browser round trip, where this API can do it
    // from here: the project is created here, and `connectRepos` puts the
    // repos in it as the grant connects them (carrick#999), so the wait is on
    // the GitHub App grant and nothing else. Without --project the step reads
    // the assignment the repos already have and offers the list, rather than
    // doing nothing at all (carrick#987).
    const prompts = { say: out.say, ask: out.ask, confirm: out.confirm, interactive, assumeYes: parsed.assumeYes };
    let project = parsed.project;
    let projectExists = false;
    if (project === null) {
      const chosen = await projectStep(credential.token, projectAssignments(initial, names), prompts);
      project = chosen.slug;
      projectExists = chosen.exists;
    } else if (!reposAreInProject(initial, names, project)) {
      projectExists = await ensureProject(credential.token, project, prompts);
    }
    const identity = await connectRepos(credential.token, names, initial, {
      interactive,
      project: project ?? undefined,
      projectExists,
      say: out.say,
    });
    // Who this machine is, and where its answers come from. The workspace slug
    // is the login: nothing on the credential or in the workspace read names a
    // user (carrick#1026).
    out.done(
      project === null
        ? `Signed in as ${identity.workspace.slug}`
        : `Signed in as ${identity.workspace.slug} · project ${project}`,
    );
    if (project !== null && !reposAreInProject(identity, names, project)) {
      // An unverified project does not end the run, whether it was named on
      // the command line or picked here. Stopping cost someone their hooks and
      // their proposal for a browser step they could only take afterwards, and
      // the documented command then needed two runs (carrick#993 row 8).
      out.warn(
        `Finish the browser steps above to put these repos in "${project}", then run carrick init --project ${project} again to verify.`,
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
    for (const line of absentRepos(identity, names, project)) out.warn(line);
  } catch (error) {
    process.stderr.write(`carrick init: ${(error as Error).message}\n`);
    return 1;
  }
  const plan = derived.plan;
  if (plan.parent_proposal) {
    const parent = plan.parent_proposal;
    const names = parent.repos.map((repo) => path.basename(repo));
    const shown = names.length > 3 ? `${names.slice(0, 3).join(", ")} and ${names.length - 3} more` : names.join(", ");
    out.warn(`The parent folder ${parent.directory} holds ${parent.repos.length} ${parent.repos.length === 1 ? "repo" : "repos"}: ${shown}. Run carrick init .. to initialise that workspace.`);
  }
  // The scanner's own warnings about what it proposed. Capped: the proposal
  // file carries every one of them, and it is named on the line above.
  const warnings = plan.repos.flatMap((repo) => repo.warnings);
  for (const warning of warnings.slice(0, 3)) out.warn(warning);
  if (warnings.length > 3) out.warn(`${warnings.length - 3} more notes on the proposal are in ${PROPOSAL_FILE}.`);
  for (const missing of plan.missing) out.warn(`Missing workspace override: ${missing}`);
  const proposed = packagesLine(plan);
  const subject = packagesFound(plan);
  if (!parsed.assumeYes) {
    if (!process.stdin.isTTY) {
      process.stderr.write("carrick init: use --yes to accept this proposal without a terminal.\n");
      return 1;
    }
    // The count is in the question, because the listing that used to stand
    // above it is gone: this is where a reader decides (carrick#1026).
    if (!await out.confirm(`Write the proposal for ${subject} and configure hooks?`)) return 0;
  }
  try {
    // The proposal is a seed for an agent, not a config: nothing derived
    // without a model is written into the repository, because the first scan
    // is the paid one and it has to run against a config someone has read
    // (carrick-cloud#799).
    writeProposal(plan.workspace, derived);
    out.done(proposed);
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
    const cleaned = other === null ? null : mergeCarrickHooks(other, null);
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
  if (mcp.length === 0) out.warn(`No agent client found on this machine. In Claude Code: ${MCP_LINE}`);

  // No paid scan ran here, and that is the point (carrick-cloud#799): the one
  // scan runs against a config someone has read. Where CI has already built an
  // index, this run reads it onto the machine instead — the hosted index is
  // the whole workspace's row, and `carrick index` from a laptop on a branch
  // replaces it for everyone who queries it (carrick#993 row 2, carrick#1020).
  if (hostedIndex.length > 0) {
    const outcome = await out.step("Reading the hosted index into .carrick/", () =>
      downloadHostedIndex(plan.workspace, nativeRunner(out.quiet)),
    );
    const report = hostedReport(outcome);
    out[report.kind](report.text);
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
