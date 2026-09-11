// Authenticated setup; Rust owns the workspace and service proposal.

import fs from "node:fs";
import path from "node:path";
import readline from "node:readline/promises";
import { spawnSync } from "node:child_process";
import { readCredential, type Credential } from "../auth/credentials.ts";
import { signIn } from "../auth/run.ts";
import { resolveRepos } from "../auth/read.ts";
import { deriveWorkspace, writeConfigs, githubRemote } from "./repos.ts";
import { connectRepos, reposAreInProject } from "./connect.ts";
import { listProjects, createProject, projectLines } from "./projects.ts";
import { connectMcpClients, mcpLines } from "./mcp.ts";
import { hookCommand, mergeCarrickHooks } from "./settings.ts";
import { renderTemplate } from "../templates.ts";
import { resolveNativeBinary, nativeEnv, packageRoot } from "../native.ts";

const EXTENSION_ID = "carrick-tools.carrick";

/**
 * The prompt that makes an agent write this repo's setup files.
 *
 * A copy, and deliberately a verbatim one: the source of truth is
 * `AGENT_SCAFFOLD_PROMPT` in the dashboard (carrick-cloud
 * `app/src/lib/onboarding.ts`), whose own test checks every filename in it
 * against what the `scaffold` MCP tool actually returns. It is printed here so
 * the terminal path ends where the dashboard path ends — with the one prompt
 * that turns a signed-in workspace into a repo that scans itself in CI
 * (carrick#955). Keep the two in step; the tool's instructions, not this text,
 * decide what gets written.
 */
const AGENT_SCAFFOLD_PROMPT =
  "Run the carrick scaffold tool, passing this repo's owner/repo from " +
  "`git remote get-url origin` as `repo`, and follow the instructions it " +
  "returns: create each file at its path, populate carrick.json by scanning " +
  "this repo for its service(s) and the env vars / domains they call (use a " +
  "nested services config if it's a monorepo), and add the Carrick section " +
  "to AGENTS.md if this repo already has one. Answer the closing checklist " +
  "before you open the PR.";

export type InitOptions = {
  workspace: string;
  /** Require every proposed GitHub repo to belong to this project. */
  project: string | null;
  /** Answer yes to the repo list rather than asking. */
  assumeYes: boolean;
  /** Write the files, print the lines, and do not build the index. */
  skipIndex: boolean;
};

export function parseArgs(argv: string[], cwd = process.cwd()): InitOptions | string {
  const options: InitOptions = { workspace: cwd, project: null, assumeYes: false, skipIndex: false };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    switch (argument) {
      case "--yes":
      case "-y":
        options.assumeYes = true;
        break;
      case "--skip-index":
        options.skipIndex = true;
        break;
      case "--project": {
        const value = argv[index + 1];
        if (!value) return "--project needs a slug";
        if (
          !/^[a-z0-9](?:[a-z0-9]|-(?=[a-z0-9])){2,31}$/.test(value)
        ) {
          return `invalid project slug "${value}": use 3-32 lowercase letters, digits, and single hyphens`;
        }
        options.project = value;
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
    "carrick init [DIRECTORY] [--project SLUG]",
    "",
    "Sign in (here, or beforehand with carrick login), then configure services,",
    "hooks, the MCP connection and the first index in a repository or a folder",
    "of repos.",
    "With --project, a project missing from the workspace is offered for",
    "creation here; the browser assigns the repos and the CLI waits for Carrick",
    "to verify the assignment.",
    "",
    "    -w, --workspace DIR  The folder holding the repos (default: this one)",
    "        --project SLUG   Require these repos in this Carrick project",
    "    -y, --yes            Take the repo list as proposed",
    "        --skip-index     Write the files and print the lines, index later",
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

/**
 * Make sure the named project exists, from the terminal where that is possible.
 *
 * Returns whether this run can state that the project exists. `false` is the
 * honest answer for every case it could not settle — an API without the
 * actions, a declined offer, a refused create — and the caller then prints
 * today's browser instructions, which include creating it.
 */
async function ensureProject(
  token: string,
  slug: string,
  options: { say: (line: string) => void; interactive: boolean; assumeYes: boolean },
): Promise<boolean> {
  const { say } = options;
  const projects = await listProjects(token);
  if (projects === null) return false;
  if (projects.some((project) => project.slug === slug && !project.archived)) {
    say(`Project "${slug}" is in this workspace.`);
    return true;
  }
  say(
    projects.length === 0
      ? "This workspace has no projects yet."
      : "Projects in this workspace:",
  );
  for (const line of projectLines(projects)) say(line);
  const create = options.assumeYes || (options.interactive && (await confirm(`Create project "${slug}"?`)));
  if (!create) return false;
  const outcome = await createProject(token, slug);
  if (outcome.kind === "created") {
    say(`Created project "${slug}".`);
    return true;
  }
  if (outcome.kind === "refused") say(`Carrick did not create "${slug}": ${outcome.message}`);
  return false;
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
  let plan: ReturnType<typeof deriveWorkspace>;
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
    plan = deriveWorkspace(workspace);
    const repoIdentities = plan.repos.map((repo) => ({ path: repo.path, name: githubRemote(repo.path) }));
    const names = [...new Set(repoIdentities.map((repo) => repo.name).filter((name): name is string => name !== null))];
    if (names.length > 200) throw new Error("This workspace has more than 200 GitHub repos. Initialise smaller workspace groups.");
    const missingIdentities = repoIdentities.filter((repo) => repo.name === null);
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
    }
    const initial = await resolveRepos(credential.token, names);
    // The project half of the browser round trip, where this API can do it
    // from here. Assignment still belongs to the browser, so this only ever
    // removes the "create the project" step from the wait.
    const projectExists =
      parsed.project && !reposAreInProject(initial, names, parsed.project)
        ? await ensureProject(credential.token, parsed.project, {
            say,
            interactive,
            assumeYes: parsed.assumeYes,
          })
        : false;
    const identity = await connectRepos(credential.token, names, initial, {
      interactive,
      project: parsed.project ?? undefined,
      projectExists,
      say,
    });
    if (parsed.project && !reposAreInProject(identity, names, parsed.project)) {
      throw new Error(
        `Project "${parsed.project}" was not verified for every requested repo. Complete the browser steps and run carrick init --project ${parsed.project} again.`,
      );
    }
    say(`Carrick workspace: ${identity.workspace.slug}`);
    if (identity.allowance_sentence) say(identity.allowance_sentence);
    for (const repo of identity.repos) {
      if (!repo.connected) say(`${repo.full_name} is not connected to this Carrick workspace.`);
      else if (repo.services.length === 0) {
        say(`${repo.full_name} is connected and has no hosted index yet.`);
        say("  Add the workflow in that repo:");
        say("    mkdir -p .github/workflows");
        say("    carrick templates workflow > .github/workflows/carrick.yml");
      }
    }
  } catch (error) {
    process.stderr.write(`carrick init: ${(error as Error).message}\n`);
    return 1;
  }
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
    if (!await confirm("Create missing configs and configure hooks?")) return 0;
  }
  try {
    for (const result of writeConfigs(plan)) say(`${result.created ? "wrote" : "unchanged"}  ${result.path}`);
    // Revalidate any file that appeared between preview and exclusive create.
    deriveWorkspace(workspace);
  } catch (error) {
    process.stderr.write(`carrick init: ${(error as Error).message}\n`);
    return 1;
  }
  say("Review carrick.json service boundaries and shared includes before committing it.");

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

  // 4. The index itself.
  if (parsed.skipIndex) {
    say();
    say("Skipped the index. Build it with: carrick index");
  } else {
    say();
    const binary = resolveNativeBinary();
    if (!binary.binary) {
      process.stderr.write(`carrick init: ${binary.problem}\n`);
      return 1;
    }
    // `--infer` is what makes the first run produce an index worth reading.
    // A local scan states the facts it can derive and leaves everything only
    // the model can classify unclassified; this run asks Carrick Cloud for
    // those, uploads the result, and writes the same payload into `.carrick`
    // from the same run — so there is no scan, then wait, then download
    // (carrick#956 §8.3). Every later `carrick index`, and every hook-driven
    // `carrick refresh`, is the free local one.
    const scan = spawnSync(binary.binary, ["index", "--workspace", workspace, "--infer"], {
      stdio: "inherit",
      env: nativeEnv(),
    });
    if (scan.status !== 0) {
      process.stderr.write(
        `carrick init: the first index did not finish. The files above are written, so fix what it reported and run: carrick index --infer\n`,
      );
      return scan.status ?? 1;
    }
  }

  // 5. The MCP connection, for work that crosses repos this machine does not
  //    hold. Configured here for every client this machine has, rather than
  //    printed for one of them (carrick#955).
  say();
  say("Next:");
  say();
  for (const line of mcpLines(connectMcpClients())) say(line);
  say();
  say("  The CI check, once per repo (it needs no secret). Paste this to your agent:");
  say();
  say(`    ${AGENT_SCAFFOLD_PROMPT}`);
  say();
  say("  Or write the workflow yourself:");
  say("    mkdir -p .github/workflows");
  say("    carrick templates workflow > .github/workflows/carrick.yml");
  say();
  say("  The first scan on the repo's default branch writes the hosted index. A session");
  say("  started in this folder afterwards picks it up on its own; by hand it is:");
  say("    carrick refresh");
  say("    carrick status");
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
  say();
  say("Then: edit a file with a route or a call in it, and Carrick answers on the edit.");
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
