// Undoing `carrick init`, on this machine.
//
// An install that can state its own undo is the courtesy that makes people
// willing to try it (carrick#1034), and init now writes in seven places: the
// hook entries in a workspace's `.claude` settings, an MCP server entry in
// each agent client's own configuration, the `.carrick` directory, the
// install id in `~/.carrick`, and the credential in the user's configuration
// directory.
//
// Every step here is the inverse of a writer in this folder, and each pair
// lives in one file so the two cannot drift: `mergeCarrickHooks` /
// `removeCarrickHooks` in `settings.ts`, `mergeServerEntry` /
// `removeServerEntry` and `connectMcpClients` / `disconnectMcpClients` in
// `mcp.ts`, `ensureInstallId` / `removeInstallId` in `install-id.ts`,
// `writeProposal` / the `.carrick` removal below, `saveCredential` /
// `removeCredential` in `../auth/credentials.ts`.
//
// What it does NOT do is edit a file whose content it cannot reproduce. The
// scaffold pull request put files in the repository and a repository is
// version controlled: those are listed with the `git rm` line that removes
// them, and the sections that were merged into files someone else owns are
// named for a human to delete. A command that rewrote a committed workflow or
// an AGENTS.md would be destroying work this command cannot read.
//
// The task skills are the one exception, and they earn it by being checkable:
// each one carries a digest of the body this package wrote, so a file that
// still matches its stamp is byte for byte what `carrick init` put there and
// nothing of anyone's is in it. One whose stamp no longer matches, and one
// carrying no stamp, are left where they are and named.
//
// It runs no scan and never asks for the scanner binary: everything it touches
// is a file this package wrote.

import fs from "node:fs";
import path from "node:path";
import { removeCredential, credentialPath } from "../auth/credentials.ts";
import { installIdPath, removeInstallId } from "./install-id.ts";
import { disconnectMcpClients, type McpRemoval } from "./mcp.ts";
import { repoRoots } from "./repos.ts";
import { removeCarrickHooks } from "./settings.ts";
import { removeTaskSkills, SKILL_ROOTS } from "./task-skills.ts";
import { createOutput, DOCS, type InitOutput } from "./output.ts";

export type RemoveOptions = {
  workspace: string;
  /** Leave the saved credential in place: this machine stays signed in. */
  keepLogin: boolean;
};

export function parseArgs(argv: string[], cwd = process.cwd()): RemoveOptions | string {
  const options: RemoveOptions = { workspace: cwd, keepLogin: false };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    switch (argument) {
      case "--keep-login":
        options.keepLogin = true;
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
        if (argument?.startsWith("-")) return `unknown option for \`carrick remove\`: ${argument}`;
        options.workspace = path.resolve(cwd, argument ?? ".");
    }
  }
  return options;
}

function help(): string {
  return [
    "carrick remove [DIRECTORY] [--keep-login]",
    "",
    "Undo what carrick init wrote on this machine: the Carrick hook entries in",
    "this folder's .claude settings, the carrick MCP server in each agent",
    "client's configuration, the .carrick directory, this machine's install id,",
    "and the saved credential.",
    "Other hooks, other MCP servers and the settings files themselves are left",
    "as they are. The task skills it wrote are removed where they still match",
    "the stamp it wrote them with, and named where they do not. Files the",
    "scaffold added to the repository are listed with the git rm line that",
    "removes them.",
    "",
    "    -w, --workspace DIR  The folder init was run in (default: this one)",
    "        --keep-login     Leave the saved credential; remove everything else",
    "",
    `What each of those things is, and what it does: ${DOCS}`,
  ].join("\n");
}

/** The settings files a workspace can hold our hook entries in. */
export const SETTINGS_FILES = [
  path.join(".claude", "settings.json"),
  path.join(".claude", "settings.local.json"),
];

/**
 * The files the scaffold pull request adds to a repository.
 *
 * The list is the scaffold tool's own (`carrick-cloud`
 * `lambdas/mcp-server/src/tools/scaffold.ts`): the workflow, the agent skill,
 * the three hook-pack scripts and the config. Only the ones that exist are
 * printed, so the `git rm` line is one a reader can paste.
 *
 * `src/git_state.rs` (`SCAFFOLD_FILES`) mirrors this list, the settings files
 * and the section carriers below, so a tree whose only changes are the
 * scaffold does not read as dirty (carrick#1117). Change both together.
 */
export const SCAFFOLD_FILES = [
  path.join(".github", "workflows", "carrick.yml"),
  path.join(".claude", "skills", "carrick", "SKILL.md"),
  path.join(".claude", "session-start.sh"),
  path.join(".claude", "turn-reminder.sh"),
  path.join(".claude", "search-gate.sh"),
  "carrick.json",
];

/** The files the scaffold merged INTO, which only their owner can unpick. */
const SECTION_CARRIERS: Array<{ file: string; holds: (body: string) => boolean; what: string }> = [
  { file: "AGENTS.md", holds: (body) => /^##\s+Carrick\s*$/m.test(body), what: 'the "## Carrick" section' },
  { file: "CLAUDE.md", holds: (body) => /^##\s+Carrick\s*$/m.test(body), what: 'the "## Carrick" section' },
  {
    file: path.join(".claude", "settings.json"),
    holds: (body) => body.includes("$CLAUDE_PROJECT_DIR/.claude/"),
    what: "the hook-pack entries the scaffold merged in",
  },
  { file: ".gitignore", holds: (body) => body.includes("!.claude/"), what: "the .claude negations" },
];

export type RepoLeftovers = {
  /** Whole files, relative to the workspace, for one `git rm`. */
  files: string[];
  /** What has to be edited by hand, one line each. */
  sections: string[];
};

/**
 * Everything the scaffold left in the repositories under this workspace.
 *
 * The repositories are `repoRoots`', which `carrick doctor` reads too. Nothing
 * here is opened unless it exists, and nothing here is changed.
 */
export function repoLeftovers(workspace: string): RepoLeftovers {
  const roots = repoRoots(workspace);

  const files: string[] = [];
  const sections: string[] = [];
  for (const root of roots) {
    for (const relative of SCAFFOLD_FILES) {
      const target = path.join(root, relative);
      if (fs.existsSync(target)) files.push(path.relative(workspace, target) || relative);
    }
    for (const carrier of SECTION_CARRIERS) {
      const target = path.join(root, carrier.file);
      let body: string;
      try {
        body = fs.readFileSync(target, "utf8");
      } catch {
        continue;
      }
      if (!carrier.holds(body)) continue;
      sections.push(`${path.relative(workspace, target) || carrier.file}: ${carrier.what}`);
    }
  }
  return { files, sections };
}

/** One `◇` line per client this run changed, and a warning for what it could not. */
export function mcpRemovalLines(removals: McpRemoval[]): { done: string[]; warn: string[] } {
  const done: string[] = [];
  const warn: string[] = [];
  for (const removal of removals) {
    if (removal.state === "removed") {
      done.push(
        removal.client === "Claude Code"
          ? `MCP server removed for ${removal.client}`
          : `MCP server removed for ${removal.client}: ${removal.detail}`,
      );
    } else if (removal.state === "kept") {
      warn.push(`${removal.client}: ${removal.detail}`);
    } else if (removal.state === "failed") {
      warn.push(`MCP server not removed for ${removal.client}: ${removal.detail}`);
    }
  }
  return { done, warn };
}

/** Strip our hook entries from one settings file. Null when there was nothing. */
function removeHooks(file: string): string | null {
  if (!fs.existsSync(file)) return null;
  const existing = fs.readFileSync(file, "utf8");
  const cleaned = removeCarrickHooks(existing);
  if (!cleaned.changed) return null;
  fs.writeFileSync(file, cleaned.body);
  return file;
}

export async function remove(argv: string[], out: InitOutput = createOutput()): Promise<number> {
  const parsed = parseArgs(argv);
  if (typeof parsed === "string") {
    process.stdout.write(`${parsed}\n`);
    return parsed.startsWith("carrick remove") ? 0 : 2;
  }
  const { workspace } = parsed;
  if (!fs.existsSync(workspace)) {
    process.stderr.write(`carrick remove: ${workspace} is not a directory on this machine\n`);
    return 1;
  }

  // Each step reports its own failure and the run carries on: a settings file
  // someone hand-edited into invalid JSON must not leave the credential and
  // the MCP entries behind.
  let removed = 0;
  for (const relative of SETTINGS_FILES) {
    const file = path.join(workspace, relative);
    try {
      if (removeHooks(file) !== null) {
        out.done(`Carrick hook entries removed from ${relative}`);
        removed += 1;
      }
    } catch (error) {
      out.refuse(`Could not read ${relative}: ${(error as Error).message}. Remove the carrick hook entries there by hand.`);
    }
  }

  const { done, warn } = mcpRemovalLines(disconnectMcpClients());
  for (const line of done) {
    out.done(line);
    removed += 1;
  }
  for (const line of warn) out.warn(line);

  // The install id goes with the entries that carried it: what the header
  // named is this machine's setup, and the setup is what just came off it. A
  // later `carrick init` mints a new one, which is the reset the file exists
  // to give (carrick-cloud#890).
  try {
    if (removeInstallId()) {
      out.done("This machine's install id removed");
      removed += 1;
    }
  } catch (error) {
    out.refuse(`${(error as Error).message}. Delete ${installIdPath()} by hand.`);
  }

  // Only the stamped, unchanged copies. Anything a user has made their own is
  // named rather than deleted, which is the same rule the install follows.
  const skills = removeTaskSkills(workspace);
  if (skills.deleted.length > 0) {
    out.done(`${skills.deleted.length} task skill file(s) removed from ${SKILL_ROOTS.join(" and ")}`);
    removed += 1;
  }
  for (const row of skills.kept) {
    out.warn(
      row.state === "edited"
        ? `${row.path} has been edited since Carrick wrote it, so it was left in place. Delete it by hand to finish removing it.`
        : `${row.path} was not written by Carrick, so it was left in place.`,
    );
  }

  const carrick = path.join(workspace, ".carrick");
  if (fs.existsSync(carrick)) {
    try {
      fs.rmSync(carrick, { recursive: true, force: true });
      out.done(".carrick removed, with the proposal and the index in it");
      removed += 1;
    } catch (error) {
      out.refuse(`Could not remove ${carrick}: ${(error as Error).message}`);
    }
  }

  if (parsed.keepLogin) {
    out.say(`This machine stays signed in; the credential is ${credentialPath()}.`);
  } else {
    try {
      if (removeCredential()) {
        out.done("Signed out: the saved credential is gone");
        removed += 1;
        out.say("Revoke the key itself at https://app.carrick.tools/account.");
      }
      if (process.env["CARRICK_TOKEN"] !== undefined) {
        out.warn("CARRICK_TOKEN still signs this shell in; unset it to finish signing out.");
      }
    } catch (error) {
      out.refuse((error as Error).message);
    }
  }

  if (removed === 0) out.say("Nothing left to remove on this machine.");

  const leftovers = repoLeftovers(workspace);
  if (leftovers.files.length > 0 || leftovers.sections.length > 0) {
    out.say("");
    out.say("In the repository, which this command does not edit:");
    if (leftovers.files.length > 0) out.say(`  git rm ${leftovers.files.join(" ")}`);
    for (const section of leftovers.sections) out.say(`  by hand — ${section}`);
  }

  out.note("Last step, when nothing else on this machine needs it", ["npm uninstall -g carrick"]);
  return 0;
}
