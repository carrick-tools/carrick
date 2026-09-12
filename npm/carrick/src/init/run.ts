// Authenticated setup; Rust owns the workspace and service proposal.

import fs from "node:fs";
import path from "node:path";
import readline from "node:readline/promises";
import { spawnSync } from "node:child_process";
import { readCredential, type Credential } from "../auth/credentials.ts";
import { signIn } from "../auth/run.ts";
import { resolveRepos } from "../auth/read.ts";
import { deriveWorkspace, writeProposal, PROPOSAL_FILE, repoIdentity } from "./repos.ts";
import { connectRepos, reposAreInProject, projectAssignments } from "./connect.ts";
import { ensureProject, projectStep, SLUG } from "./projects.ts";
import { connectMcpClients, mcpLines } from "./mcp.ts";
import { hookCommand, mergeCarrickHooks } from "./settings.ts";
import { renderTemplate } from "../templates.ts";
import { packageRoot } from "../native.ts";

const EXTENSION_ID = "carrick-tools.carrick";

/**
 * The prompt that makes an agent write this repo's config and setup files.
 *
 * A copy, and deliberately a verbatim one: the source of truth is the
 * `scaffold` MCP tool's own instructions and the dashboard copy beside them
 * (carrick-cloud#800), whose test checks every filename against what the tool
 * returns. It is printed here so the terminal path ends where the dashboard
 * path ends, and it states the sequence the ruling in carrick-cloud#799 fixed:
 * the agent writes a complete `carrick.json` from the proposal this command
 * derived, `carrick index` proves it for nothing, and only then does the one
 * paid scan run. Keep the two copies in step; the tool's instructions, not
 * this text, decide what gets written.
 */
export const AGENT_SCAFFOLD_PROMPT =
  "Run the carrick scaffold tool, passing this repo's owner/repo from " +
  "`git remote get-url origin` as `repo`, and follow the instructions it " +
  "returns: create each file at its path, and write carrick.json from the " +
  "proposal in .carrick/proposal.json, taking applications as services and " +
  "library workspace members as shared includes of the services that import " +
  "them, with the env vars and domains each service calls. Add the Carrick " +
  "section to AGENTS.md if this repo already has one. Then run `carrick " +
  "index`, which is free and runs no model, fix whatever it reports as " +
  "unclassified or in no service, and run `carrick index --infer` once for " +
  "the scan that builds the index. Answer the closing checklist before you " +
  "open the PR.";

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
    ".carrick directory and the hook settings, and it runs no scan.",
    "With --project, a project missing from the workspace is offered for",
    "creation here; the browser assigns the repos and the CLI waits for Carrick",
    "to verify the assignment.",
    "",
    "    -w, --workspace DIR  The folder holding the repos (default: this one)",
    "        --project SLUG   Require these repos in this Carrick project",
    "        --repo OWNER/REPO  Name the GitHub repo whose origin remote names none",
    "    -y, --yes            Take the repo list as proposed",
  ].join("\n");
}

function say(line = ""): void {
  process.stdout.write(`${line}\n`);
}

async function confirm(question: string): Promise<boolean> {
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
  try {
    const answer = await rl.question(`${question} [Y/n] `);
    return answer.trim() === "" || /^y(es)?$/i.test(answer.trim());
  } finally {
    rl.close();
  }
}

/** A typed answer, for the questions whose answer is not yes or no. */
async function ask(question: string): Promise<string> {
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
  try {
    return (await rl.question(`${question}\n> `)).trim();
  } finally {
    rl.close();
  }
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
 * The editor lines, one line per editor that is on the machine.
 *
 * `--install-extension <id>` resolves the id against that editor's own gallery,
 * and the three editors below read three different ones: VS Code the VS Code
 * Marketplace, Cursor and Windsurf each their own. The extension is published
 * to all three, so the id is a command every one of them can run
 * (carrick#915). Any other editor gets the server's command and no claim about
 * the editor: the per-editor results table in `plugin/TEST-PLAN.md` section 6
 * is still empty.
 *
 * `onPath` is injected so a test can state each machine.
 */
export function editorLines(onPath: (command: string) => boolean): string[] {
  const lines: string[] = [];
  for (const [command, editor] of [
    ["code", "VS Code"],
    ["cursor", "Cursor"],
    ["windsurf", "Windsurf"],
  ] as const) {
    if (!onPath(command)) continue;
    lines.push(`  ${editor}, for diagnostics in the Problems panel:`);
    lines.push(`    ${command} --install-extension ${EXTENSION_ID}`);
  }
  if (lines.length === 0) {
    lines.push("  Any editor with an LSP client starts the server itself as `carrick lsp --stdio`.");
  }
  return lines;
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

  // Authentication and all derivation validation precede local writes.
  let derived: ReturnType<typeof deriveWorkspace>;
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
      say("This machine is not signed in to Carrick.");
      credential = await signIn(say);
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
        say(`--repo ${parsed.repo} was not needed: every repo here names its own GitHub repository.`);
      } else {
        taken = unnamed[0]!.path;
        say(`Taking ${parsed.repo} as the GitHub repository for ${taken}.`);
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
      say(`${repo.path} contributes no GitHub identity: ${repo.problem}.`);
    }
    if (missingIdentities.length > 10) {
      say(`${missingIdentities.length - 10} more repos here name no GitHub identity either.`);
    }
    if (missingIdentities.length > 0) {
      const one = missingIdentities.length === 1;
      say(
        `  Carrick has nothing to connect ${one ? "it" : "them"} to. Run carrick init --repo owner/repo${one ? "" : " in each of them"}, or give the alias a HostName github.com line in your ssh config.`,
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
    if (parsed.project) {
      say(`Repos requested for project "${parsed.project}":`);
      for (const name of names) say(`  ${name}`);
    } else if (names.length === 0) {
      // The sentence that stops the rest of this run from reading as a
      // complete one: no project is chosen, no connection is checked, and a
      // later upload has no repo identity to resolve a project from.
      say("No repo here names a GitHub repository, so this run chooses no project and checks no connection.");
    }
    const initial = await resolveRepos(credential.token, names);
    // The project half of the browser round trip, where this API can do it
    // from here. Assignment still belongs to the browser, so this only ever
    // removes the "create the project" step from the wait. Without --project
    // the step reads the assignment the repos already have and offers the
    // list, rather than doing nothing at all (carrick#987).
    const prompts = { say, ask, confirm, interactive, assumeYes: parsed.assumeYes };
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
      say,
    });
    if (project !== null && !reposAreInProject(identity, names, project)) {
      // A project NAMED on the command line is a requirement, and an
      // unverified one fails the run. A project picked during the step is not:
      // stopping there would cost someone their hooks and their proposal for
      // answering a question they were offered (carrick#987).
      if (parsed.project !== null) {
        throw new Error(
          `Project "${project}" was not verified for every requested repo. Complete the browser steps and run carrick init --project ${project} again.`,
        );
      }
      say(`Setup continues; finish the browser steps to put these repos in "${project}".`);
    }
    say(`Carrick workspace: ${identity.workspace.slug}`);
    if (identity.allowance_sentence) say(identity.allowance_sentence);
    for (const repo of identity.repos) {
      if (!repo.connected) say(`${repo.full_name} is not connected to this Carrick workspace.`);
      // How to give it one is the last thing this command prints, once, rather
      // than a workflow line per repo before anything is set up.
      else if (repo.services.length === 0) say(`${repo.full_name} is connected and has no hosted index yet.`);
    }
  } catch (error) {
    process.stderr.write(`carrick init: ${(error as Error).message}\n`);
    return 1;
  }
  const plan = derived.plan;
  say(`Repos to index, in ${plan.workspace} (${plan.repos_detected_by}):`);
  if (plan.parent_proposal) {
    const parent = plan.parent_proposal;
    // Reads like the scanner's own sentence for the same proposal, capped the
    // same way: a folder of scratch checkouts holds dozens, and the folder
    // that holds them is already named here.
    const names = parent.repos.map((repo) => path.basename(repo));
    const shown = names.length > 3 ? `${names.slice(0, 3).join(", ")} and ${names.length - 3} more` : names.join(", ");
    say(`The parent folder ${parent.directory} holds ${parent.repos.length} ${parent.repos.length === 1 ? "repo" : "repos"}: ${shown}. Run carrick init .. to initialise that workspace.`);
  }
  for (const repo of plan.repos) {
    say(`  ${repo.path} (${repo.reason})`);
    for (const service of repo.services) {
      say(`    ${service.serviceName ?? "<repository>"}: directory ${service.directory ?? "."}, tsconfig ${service.tsconfig ?? "scanner default"}`);
    }
    for (const warning of repo.warnings) say(`    ${warning}`);
  }
  for (const missing of plan.missing) say(`Missing workspace override: ${missing}`);
  if (!parsed.assumeYes) {
    if (!process.stdin.isTTY) {
      process.stderr.write("carrick init: use --yes to accept this proposal without a terminal.\n");
      return 1;
    }
    if (!await confirm("Write this proposal and configure hooks?")) return 0;
  }
  try {
    // The proposal is a seed for an agent, not a config: nothing derived
    // without a model is written into the repository, because the first scan
    // is the paid one and it has to run against a config someone has read
    // (carrick-cloud#799).
    say(`wrote  ${writeProposal(plan.workspace, derived)}`);
  } catch (error) {
    process.stderr.write(`carrick init: ${(error as Error).message}\n`);
    return 1;
  }

  // 3. The agent hooks. Merged by command: this file may already hold a user's
  //    own hooks, or the hook pack the hosted index installs.
  const command = hookCommand({ onPath });
  const settingsName = path.join(".claude", command.bare ? "settings.json" : "settings.local.json");
  const settingsFile = path.join(workspace, settingsName);
  try {
    const settings = fs.existsSync(settingsFile) ? fs.readFileSync(settingsFile, "utf8") : null;
    const otherFile = path.join(workspace, ".claude", command.bare ? "settings.local.json" : "settings.json");
    const other = fs.existsSync(otherFile) ? fs.readFileSync(otherFile, "utf8") : null;
    const cleaned = other === null ? null : mergeCarrickHooks(other, null);
    const hooks = mergeCarrickHooks(settings, command.command);
    // Validate both documents before migrating our entries between them.
    if (cleaned?.changed) writeIfChanged(otherFile, cleaned.body);
    const wroteHooks = writeIfChanged(settingsFile, hooks.body);
    say(`${wroteHooks === "written" ? "wrote" : "unchanged"}  ${settingsName}`);
    if (!command.bare) {
      say(
        `         \`carrick\` is not on PATH here, so those hooks name this install: ${command.command}.`,
      );
      say(
        "         An npx run leaves nothing on PATH afterwards, and a hook that cannot find carrick",
      );
      say(
        "         says nothing rather than failing your edit. `npm install -g carrick` and run init",
      );
      say("         again to write the short command instead.");
    }
  } catch (error) {
    say(
      `Could not configure Carrick hooks: ${(error as Error).message}. Fix the settings files and run carrick init again.`,
    );
  }

  // 4. The MCP connection, for work that crosses repos this machine does not
  //    hold. Configured here for every client this machine has, rather than
  //    printed for one of them (carrick#955).
  say();
  say("Next:");
  say();
  for (const line of mcpLines(connectMcpClients())) say(line);
  say();
  for (const line of editorLines(onPath)) say(line);
  say();
  say(`Start Claude Code in this folder — the hooks above are ${settingsName} here, and`);
  say("the index covers every repo in it. The hooks need no plugin; the language server does:");
  const plugin = path.join(packageRoot(), "plugin");
  say(`    claude --plugin-dir ${fs.existsSync(plugin) ? plugin : "<carrick checkout>/plugin"}`);
  if (!command.bare) {
    say(
      "    That plugin's language server is started as `carrick`, which this machine cannot resolve;",
    );
    say("    install it globally first, or the hooks above are the channel.");
  }
  say("Once the index exists, editing a file with a route or a call in it is what");
  say("Carrick answers on; `carrick status` says what the index holds.");
  say();
  // No scan ran here, and that is the point (carrick-cloud#799): the one paid
  // scan runs against a config someone has read, so the last thing this
  // command prints is the prompt that produces that config.
  say(`  There is no index yet. ${PROPOSAL_FILE} holds the services this run derived;`);
  say("  an agent turns it into carrick.json, adds the CI check (which needs no secret),");
  say("  and builds the index: `carrick index` is free and runs no model, and");
  say("  `carrick index --infer` is the single scan that asks Carrick to classify the rest.");
  say("  By hand instead: `carrick templates workflow > .github/workflows/carrick.yml`, a");
  say("  carrick.json per https://docs.carrick.tools/carrick-json, then those two commands.");
  say();
  say("  Paste this to your agent:");
  say();
  say(`    ${AGENT_SCAFFOLD_PROMPT}`);
  return 0;
}

/** `carrick templates <name>`, so the workflow above is one command. */
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
