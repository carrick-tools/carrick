// `carrick init`: the one command, run in the folder that holds the repos.
//
// It does the four things that are otherwise four installs — the repo list,
// the first index, the agent hooks, and the editor line — and prints the two
// it cannot do for you. Everything it writes is a file you can read and edit
// afterwards; nothing here is state only Carrick understands.
//
// Re-running it updates rather than duplicates: the repo list keeps the order
// and the hand-written entries it already had, and the hook entries are merged
// by command rather than replacing the settings file's `hooks` key.

import fs from "node:fs";
import path from "node:path";
import readline from "node:readline/promises";
import { spawnSync } from "node:child_process";
import { describeIdentity, githubIdentity } from "./identity.ts";
import { findRepos, mergeWorkspace } from "./repos.ts";
import { mergeCarrickHooks } from "./settings.ts";
import { renderTemplate } from "../templates.ts";
import { resolveNativeBinary, nativeEnv } from "../native.ts";

const WORKSPACE_FILE = "carrick-workspace.json";
const SETTINGS_FILE = path.join(".claude", "settings.json");
const MCP_LINE = "claude mcp add --scope user --transport http carrick https://api.carrick.tools/mcp";

export type InitOptions = {
  workspace: string;
  /** Answer yes to the repo list rather than asking. */
  assumeYes: boolean;
  /** Write the files, print the lines, and do not build the index. */
  skipIndex: boolean;
};

export function parseArgs(argv: string[], cwd = process.cwd()): InitOptions | string {
  const options: InitOptions = { workspace: cwd, assumeYes: false, skipIndex: false };
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
    "carrick init [DIRECTORY]",
    "",
    "Set up the folder that holds your repos: the list to index, the first index,",
    "and the wiring that puts its answers where you work.",
    "",
    "    -w, --workspace DIR  The folder holding the repos (default: this one)",
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

  // 1. Who is asking. There is no anonymous tier: the local index draws on the
  //    identified free tier's allowance, so this is the first question and not
  //    a thing to discover halfway through a scan.
  const lookup = githubIdentity();
  if (!lookup.identity) {
    process.stderr.write(`${lookup.problem}\n`);
    return 1;
  }
  say(describeIdentity(lookup.identity));
  say();

  // 2. The repos, proposed and confirmed.
  const workspaceFile = path.join(workspace, WORKSPACE_FILE);
  const existing = fs.existsSync(workspaceFile) ? fs.readFileSync(workspaceFile, "utf8") : null;
  const found = findRepos(workspace);
  const merged = mergeWorkspace(existing, found);
  if (merged.repos.length === 0) {
    process.stderr.write(
      `carrick init: no repos in ${workspace}. Carrick indexes the directories beside each other, so run it in the folder that holds them, or pass one: carrick init ~/code\n`,
    );
    return 1;
  }
  say(`Repos to index, in ${workspace}:`);
  for (const repo of merged.repos) {
    say(`  ${repo}${merged.added.includes(repo) ? "  (new)" : ""}`);
  }
  say();
  if (!parsed.assumeYes && merged.added.length > 0) {
    const ok = await confirm("Index these?");
    if (!ok) {
      say(`Nothing written. Edit ${WORKSPACE_FILE} yourself and run carrick index.`);
      return 0;
    }
  }
  const wrote = writeIfChanged(workspaceFile, merged.body);
  say(`${wrote === "written" ? "wrote" : "unchanged"}  ${WORKSPACE_FILE}`);

  // 3. The agent hooks. Merged by command: this file may already hold a user's
  //    own hooks, or the hook pack the hosted index installs.
  const settingsFile = path.join(workspace, SETTINGS_FILE);
  const settings = fs.existsSync(settingsFile) ? fs.readFileSync(settingsFile, "utf8") : null;
  try {
    const hooks = mergeCarrickHooks(settings);
    const wroteHooks = writeIfChanged(settingsFile, hooks.body);
    say(`${wroteHooks === "written" ? "wrote" : "unchanged"}  ${SETTINGS_FILE}`);
  } catch (error) {
    say(
      `skipped  ${SETTINGS_FILE}: it is not valid JSON (${(error as Error).message}). Fix it and run carrick init again; nothing was overwritten.`,
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
    const scan = spawnSync(binary.binary, ["index", "--workspace", workspace], {
      stdio: "inherit",
      env: nativeEnv(),
    });
    if (scan.status !== 0) {
      process.stderr.write(
        `carrick init: the first index did not finish. The files above are written, so fix what it reported and run: carrick index\n`,
      );
      return scan.status ?? 1;
    }
  }

  // 5. The lines it cannot run for you.
  say();
  say("Next, and neither of these is done for you:");
  say();
  say("  The org index, for work that crosses repos you do not have on disk:");
  say(`    ${MCP_LINE}`);
  say();
  say("  The CI check, once per repo (it needs no secret):");
  say("    carrick templates workflow > .github/workflows/carrick.yml");
  say();
  if (onPath("code")) {
    say("  VS Code, for diagnostics in the Problems panel:");
    say("    code --install-extension carrick-tools.carrick");
    say();
  }
  say("Claude Code reads the hooks above with no plugin. For the language server:");
  say(`    claude --plugin-dir <this checkout>/plugin`);
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
