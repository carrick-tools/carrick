// Undoing `carrick init`, in two halves (carrick#1573).
//
// `carrick remove` undoes what init wrote in one folder: the hook entries in
// that folder's `.claude` settings and in `.codex/hooks.json`, the task skills
// under `.claude/skills` and `.agents/skills`, a copy of those hooks and skills
// inside each repo of a folder with the `.git/info/exclude` lines that keep it
// out of git (`writeRepoCopy` / `removeRepoCopy` in `repo-copies.ts`), the repo
// selection in `carrick-workspace.json`, and the `.carrick` directory.
//
// `carrick uninstall` undoes what init wrote on this machine, which every
// folder set up on it shares: an MCP server entry in each agent client's own
// configuration, the install id, the session records and the refresh notice in
// `~/.carrick`, and the credential in the user's configuration directory.
//
// They were one command, and running it to reset one folder disconnected the
// agent in every other folder on the machine and signed the machine out. Each
// now prints the whole list of what it will delete, as `init` does before "Go
// ahead?", and deletes nothing until it has a typed answer: the folder's name
// for `remove`, the word `uninstall` for `uninstall`, as the dashboard's
// project delete asks for the slug. Without a terminal the answer is
// `--confirm <answer>`, never `--yes`: to `init`, `--yes` means "take the
// proposal", and an agent adds it by reflex.
//
// Every step here is the inverse of a writer in this folder, and each pair
// lives in one file so the two cannot drift: `mergeCarrickHooks` /
// `removeCarrickHooks` in `settings.ts`, `writeTaskSkills` /
// `removeTaskSkills` in `task-skills.ts`, `mergeServerEntry` /
// `removeServerEntry` and `connectMcpClients` / `disconnectMcpClients` in
// `mcp.ts`, `ensureInstallId` / `removeInstallId` in `install-id.ts`,
// `writeProposal` / the `.carrick` removal below, `saveCredential` /
// `removeCredential` in `../auth/credentials.ts`. The list each command prints
// is made by the read half of the same pairs (`recordedRepoCopy`,
// `inspectTaskSkills`, `withoutOurExclusions`, `inspectMcpClients`), and the
// removal then acts on what the list named and nothing else.
//
// What neither does is edit a file whose content it cannot reproduce. The
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
// Neither runs a scan or asks for the scanner binary: everything they touch is
// a file this package wrote.

import fs from "node:fs";
import path from "node:path";
import { removeCredential, credentialPath } from "../auth/credentials.ts";
import { installIdPath, removeInstallId } from "./install-id.ts";
import { removeSessions, sessionRecords, sessionsDir } from "../hook/reuse.ts";
import { disconnectMcpClients, inspectMcpClients, type McpRemoval } from "./mcp.ts";
import { repoRoots } from "./repos.ts";
import { removeCarrickHooks, SETTINGS_FILES } from "./settings.ts";
import { CODEX_HOOKS_FILE, isEmptyHooksDocument, readHooksFile, uninstallCodexHooks } from "./codex.ts";
import { inspectTaskSkills, removeTaskSkills, SKILL_ROOTS } from "./task-skills.ts";
import { noticeFile, removeNotice } from "./outdated.ts";
import { readWorkspaceFile, removeSelection, withoutOurExclusions, WORKSPACE_FILE } from "./workspace-file.ts";
import { recordedRepoCopy, removeRepoCopy } from "./repo-copies.ts";
import { createOutput, DOCS_INIT_FILES, listBlock, PromptCancelled, tilde, type InitOutput } from "./output.ts";

export type RemoveOptions = {
  workspace: string;
  /** The answer to the question, given up front: this folder's name. */
  confirm: string | null;
};

export type UninstallOptions = {
  /** Leave the saved credential in place: this machine stays signed in. */
  keepLogin: boolean;
  /** The answer to the question, given up front: the word `uninstall`. */
  confirm: string | null;
};

/** The word `carrick uninstall` asks to have typed. */
export const UNINSTALL_ANSWER = "uninstall";

export function parseRemoveArgs(argv: string[], cwd = process.cwd()): RemoveOptions | string {
  const options: RemoveOptions = { workspace: cwd, confirm: null };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    switch (argument) {
      case "--confirm": {
        const value = argv[index + 1];
        if (value === undefined) return "--confirm needs this folder's name";
        options.confirm = value;
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
        return removeHelp();
      default:
        if (argument?.startsWith("-")) return `unknown option for \`carrick remove\`: ${argument}`;
        options.workspace = path.resolve(cwd, argument ?? ".");
    }
  }
  return options;
}

export function parseUninstallArgs(argv: string[]): UninstallOptions | string {
  const options: UninstallOptions = { keepLogin: false, confirm: null };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    switch (argument) {
      case "--keep-login":
        options.keepLogin = true;
        break;
      case "--confirm": {
        const value = argv[index + 1];
        if (value === undefined) return `--confirm needs the word ${UNINSTALL_ANSWER}`;
        options.confirm = value;
        index += 1;
        break;
      }
      case "--help":
      case "-h":
        return uninstallHelp();
      default:
        if (argument?.startsWith("-")) return `unknown option for \`carrick uninstall\`: ${argument}`;
        return `\`carrick uninstall\` takes no directory: it acts on this machine. \`carrick remove ${argument}\` acts on one folder.`;
    }
  }
  return options;
}

function removeHelp(): string {
  return [
    "carrick remove [DIRECTORY] [--confirm NAME]",
    "",
    "Undo what carrick init wrote in this folder, and nowhere else: the Carrick",
    "hook entries in its .claude settings and in .codex/hooks.json, the hooks and",
    "skills it copied into each repo here with their .git/info/exclude lines, the",
    "repo selection in carrick-workspace.json, and the .carrick directory.",
    "Other hooks and the settings files themselves are left as they are, and so",
    "is anything in carrick-workspace.json that init did not put there. The task",
    "skills it wrote are removed where they still match the stamp it wrote them",
    "with, and named where they do not. Files the scaffold added to the",
    "repository are listed with the git rm line that removes them.",
    "",
    "It lists what it will delete, then asks for this folder's name. The MCP",
    "server, the install id and the sign-in belong to this machine and stay;",
    "carrick uninstall removes them.",
    "",
    "    -w, --workspace DIR  The folder init was run in (default: this one)",
    "        --confirm NAME   This folder's name, as the answer to the question.",
    "                         Required where there is no terminal to ask in",
    "",
    `What each of those things is, and what it does: ${DOCS_INIT_FILES}`,
  ].join("\n");
}

function uninstallHelp(): string {
  return [
    `carrick uninstall [--keep-login] [--confirm ${UNINSTALL_ANSWER}]`,
    "",
    "Undo what carrick init wrote on this machine, which every folder set up on",
    "it shares: the carrick MCP server in each agent client's configuration, this",
    "machine's install id, the session records and the refresh notice in",
    "~/.carrick, and the saved credential. Other MCP servers are left as they",
    "are. Each folder keeps its hooks and skills; carrick remove, run in that",
    "folder, takes them out.",
    "",
    `It lists what it will delete, then asks you to type ${UNINSTALL_ANSWER}.`,
    "",
    "        --keep-login     Leave the saved credential; remove everything else",
    `        --confirm WORD   ${UNINSTALL_ANSWER}, as the answer to the question. Required`,
    "                         where there is no terminal to ask in",
    "",
    `What each of those things is, and what it does: ${DOCS_INIT_FILES}`,
  ].join("\n");
}

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

/** A repo under the folder as the reader knows it: its path from the folder, or its own name. */
function repoName(workspace: string, repo: string): string {
  return path.relative(workspace, repo) || path.basename(repo);
}

/** The answer `carrick remove` asks for: the folder's own name. */
export function folderName(workspace: string): string {
  return path.basename(workspace) || workspace;
}

/** A word as it would be typed in a shell, quoted only where it has to be. */
function typed(word: string): string {
  return /^[\w.@%+=:,/-]+$/.test(word) ? word : `'${word.replaceAll("'", `'\\''`)}'`;
}

/**
 * What `carrick remove` found in a folder, before it deletes any of it.
 *
 * Each field is one kind of thing the removal takes, read by the read half of
 * its writer's pair, and the removal acts on these fields and nothing else: a
 * file listed here is a file it removes from, and a file it could not read is
 * one of `leaves`, said above the question and not tried again.
 */
export type FolderPlan = {
  workspace: string;
  /** The repos here holding a copy init recorded in their exclude file. */
  copies: string[];
  /** The settings files holding entries of ours, relative to the folder. */
  settings: string[];
  /** Our entries in `.codex/hooks.json`; `file` where they are all it holds. */
  codex: "entries" | "file" | null;
  /** The task skills still as init wrote them, relative to the folder. */
  skills: string[];
  /** The names init added to the exclude list; `file` where they are all it holds. */
  selection: { names: string[]; file: boolean } | null;
  carrick: boolean;
  /** What stays, and why. */
  leaves: string[];
};

export function folderPlan(workspace: string): FolderPlan {
  const plan: FolderPlan = {
    workspace,
    copies: [],
    settings: [],
    codex: null,
    skills: [],
    selection: null,
    carrick: false,
    leaves: [],
  };

  // A copy is taken back whole, and first: this folder may itself be one of
  // the repos, and its copy's files are then not listed a second time below
  // (carrick#1512).
  const covered = new Set<string>();
  for (const repo of repoRoots(workspace)) {
    try {
      const copy = recordedRepoCopy(repo);
      if (copy === null) continue;
      plan.copies.push(repo);
      for (const relative of copy.recorded) covered.add(path.join(repo, relative));
    } catch (error) {
      plan.leaves.push(
        `Could not read the .git/info/exclude of ${repoName(workspace, repo)}: ${(error as Error).message}. Its copy of Carrick's hooks and skills stays.`,
      );
    }
  }

  for (const relative of SETTINGS_FILES) {
    const file = path.join(workspace, relative);
    if (covered.has(file) || !fs.existsSync(file)) continue;
    try {
      if (removeCarrickHooks(fs.readFileSync(file, "utf8")).changed) plan.settings.push(relative);
    } catch (error) {
      plan.leaves.push(`Could not read ${relative}: ${(error as Error).message}. Remove the carrick hook entries there by hand.`);
    }
  }

  const codex = covered.has(path.join(workspace, CODEX_HOOKS_FILE)) ? null : readHooksFile(workspace);
  if (codex !== null) {
    try {
      const cleaned = removeCarrickHooks(codex);
      if (cleaned.changed) plan.codex = isEmptyHooksDocument(cleaned.body) ? "file" : "entries";
    } catch (error) {
      plan.leaves.push(
        `Could not read ${CODEX_HOOKS_FILE}: ${(error as Error).message}. Remove the carrick hook entries there by hand.`,
      );
    }
  }

  // Only the stamped, unchanged copies. Anything a user has made their own is
  // named rather than deleted, which is the same rule the install follows.
  for (const skill of inspectTaskSkills(workspace)) {
    if (skill.state === "ours") {
      if (!covered.has(path.join(workspace, skill.path))) plan.skills.push(skill.path);
    } else if (skill.state === "edited") {
      plan.leaves.push(`${skill.path} has been edited since Carrick wrote it, so it stays. Delete it by hand to finish removing it.`);
    } else if (skill.state === "theirs") {
      plan.leaves.push(`${skill.path} was not written by Carrick, so it stays.`);
    }
  }

  // The repo selection, which is the one thing init writes into a file a user
  // can also write to (carrick#1344). Only the names init put in the exclude
  // list come out; a repo somebody excluded by hand stays excluded, and the
  // file stays unless init is the whole reason it is there.
  const selection = readWorkspaceFile(workspace);
  if (selection !== null) {
    try {
      const cleaned = withoutOurExclusions(selection);
      if (cleaned.removed.length > 0) plan.selection = { names: cleaned.removed, file: cleaned.empty };
    } catch (error) {
      plan.leaves.push(`Could not read ${WORKSPACE_FILE}: ${(error as Error).message}. Take the carrick exclusions out by hand.`);
    }
  }

  plan.carrick = fs.existsSync(path.join(workspace, ".carrick"));
  return plan;
}

/** The list `carrick remove` prints before it asks: one line per thing it deletes. */
export function folderLines(plan: FolderPlan): string[] {
  const lines = plan.copies.map(
    (repo) => `Carrick's hooks and skills in ${repoName(plan.workspace, repo)}, with its .git/info/exclude lines`,
  );
  for (const relative of plan.settings) lines.push(`Carrick's hook entries in ${relative}`);
  if (plan.codex === "file") lines.push(`${CODEX_HOOKS_FILE}, which holds only Carrick's hook entries`);
  if (plan.codex === "entries") lines.push(`Carrick's hook entries in ${CODEX_HOOKS_FILE}`);
  if (plan.skills.length > 0) lines.push(`${plan.skills.length} task skill file(s) in ${SKILL_ROOTS.join(" and ")}`);
  if (plan.selection !== null) {
    lines.push(
      plan.selection.file
        ? `${WORKSPACE_FILE}, which holds only the repo selection init wrote`
        : `${plan.selection.names.join(", ")} from the exclude list in ${WORKSPACE_FILE}`,
    );
  }
  if (plan.carrick) lines.push(".carrick, with the proposal and the index in it");
  return lines;
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

/**
 * Delete what the list named. Each step reports its own failure and the run
 * carries on: a file that cannot be written must not leave the rest behind.
 */
function removeFolder(plan: FolderPlan, out: InitOutput): void {
  const { workspace } = plan;

  // The copies first, for the reason `folderPlan` gives: the cleanup below
  // would otherwise empty a copy's settings file and leave it, and then the
  // copy's removal would take away the line that kept it out of git.
  for (const repo of plan.copies) {
    const name = repoName(workspace, repo);
    try {
      if (removeRepoCopy(repo) !== null) {
        out.done(`Carrick's hooks and skills removed from ${name}, with its .git/info/exclude lines`);
      }
    } catch (error) {
      out.refuse(`Could not remove Carrick's hooks and skills from ${name}: ${(error as Error).message}`);
    }
  }

  for (const relative of plan.settings) {
    try {
      if (removeHooks(path.join(workspace, relative)) !== null) out.done(`Carrick hook entries removed from ${relative}`);
    } catch (error) {
      out.refuse(`Could not read ${relative}: ${(error as Error).message}. Remove the carrick hook entries there by hand.`);
    }
  }

  // Codex's half of the same pair. Unlike a settings file, this one exists
  // because init wrote it, so a file left holding no hooks at all goes with the
  // entries (carrick#1335).
  if (plan.codex !== null) {
    try {
      const codex = uninstallCodexHooks(workspace);
      if (codex !== null) {
        out.done(
          codex === "file removed"
            ? `${CODEX_HOOKS_FILE} removed, with the Carrick hook entries that were the whole of it`
            : `Carrick hook entries removed from ${CODEX_HOOKS_FILE}`,
        );
      }
    } catch (error) {
      out.refuse(
        `Could not read ${CODEX_HOOKS_FILE}: ${(error as Error).message}. Remove the carrick hook entries there by hand.`,
      );
    }
  }

  if (plan.skills.length > 0) {
    try {
      const skills = removeTaskSkills(workspace, (relative) => plan.skills.includes(relative));
      if (skills.deleted.length > 0) {
        out.done(`${skills.deleted.length} task skill file(s) removed from ${SKILL_ROOTS.join(" and ")}`);
      }
    } catch (error) {
      out.refuse(`Could not remove the task skills: ${(error as Error).message}`);
    }
  }

  if (plan.selection !== null) {
    try {
      const selection = removeSelection(workspace);
      if (selection !== null) {
        out.done(
          selection.deleted
            ? `${selection.file} removed, with the repo selection init wrote in it`
            : `${selection.removed.join(", ")} taken out of the exclude list in ${selection.file}; the rest of the file is yours and was left`,
        );
      }
    } catch (error) {
      out.refuse(`Could not read ${WORKSPACE_FILE}: ${(error as Error).message}. Take the carrick exclusions out by hand.`);
    }
  }

  if (plan.carrick) {
    const carrick = path.join(workspace, ".carrick");
    try {
      fs.rmSync(carrick, { recursive: true, force: true });
      out.done(".carrick removed, with the proposal and the index in it");
    } catch (error) {
      out.refuse(`Could not remove ${carrick}: ${(error as Error).message}`);
    }
  }
}

/**
 * What `carrick uninstall` found on this machine, before it deletes any of it.
 * The same contract as `FolderPlan`: the removal acts on these and nothing else.
 */
export type MachinePlan = {
  /** The agent clients holding an MCP server entry of ours. */
  mcp: string[];
  sessions: number;
  notice: boolean;
  installId: boolean;
  /** The credential file, where this run deletes it. */
  credential: string | null;
  /** What stays, and why. */
  leaves: string[];
};

export function machinePlan(keepLogin: boolean): MachinePlan {
  const plan: MachinePlan = {
    mcp: [],
    sessions: sessionRecords(),
    notice: fs.existsSync(noticeFile()),
    installId: fs.existsSync(installIdPath()),
    credential: null,
    leaves: [],
  };
  // Gated on the URL, never the name: a server called `carrick` pointing at
  // another host was not written by this package, and stays.
  for (const client of inspectMcpClients()) {
    if (client.state === "connected") plan.mcp.push(client.client);
    else if (client.state === "elsewhere") plan.leaves.push(`${client.client}: ${client.detail}, so it stays.`);
    else if (client.state === "unreadable") plan.leaves.push(`${client.client}: ${client.detail}. Remove "carrick" there by hand.`);
  }
  if (!keepLogin) {
    try {
      const file = credentialPath();
      if (fs.existsSync(file)) plan.credential = file;
    } catch (error) {
      plan.leaves.push((error as Error).message);
    }
    if (process.env["CARRICK_TOKEN"] !== undefined) {
      plan.leaves.push("CARRICK_TOKEN still signs this shell in; unset it to finish signing out.");
    }
  }
  return plan;
}

/** The list `carrick uninstall` prints before it asks. */
export function machineLines(plan: MachinePlan): string[] {
  const lines = plan.mcp.map((client) => `Carrick's MCP server in ${client}`);
  if (plan.sessions > 0) lines.push(`${plan.sessions} session record(s) in ${tilde(sessionsDir())}`);
  if (plan.notice) lines.push(`The refresh notice's last-shown date, in ${tilde(noticeFile())}`);
  if (plan.installId) lines.push(`This machine's install id, in ${tilde(installIdPath())}`);
  if (plan.credential !== null) {
    lines.push(`The saved credential, in ${tilde(plan.credential)}, which signs this machine out`);
  }
  return lines;
}

function removeMachine(plan: MachinePlan, out: InitOutput): void {
  if (plan.mcp.length > 0) {
    // A `kept` entry was named above the question already.
    const { done, warn } = mcpRemovalLines(disconnectMcpClients().filter((removal) => removal.state !== "kept"));
    for (const line of done) out.done(line);
    for (const line of warn) out.warn(line);
  }

  // Before the install id, so the directory both live in can go with it: what
  // the Stop hook speaks from is a scratch record of conversations that have
  // ended, and it is no more use than the hook entry that fed it
  // (carrick#1330).
  if (plan.sessions > 0) {
    const sessions = removeSessions();
    if (sessions > 0) out.done(`${sessions} session record(s) removed from ${tilde(sessionsDir())}`);
  }
  // Same directory, same reason, and the same ordering it imposes: the day the
  // refresh notice was last said means nothing once there is no install to
  // refresh (carrick#1333).
  if (plan.notice && removeNotice()) out.done("The refresh notice's last-shown date removed");

  // The install id goes with the entries that carried it: what the header
  // named is this machine's setup, and the setup is what just came off it. A
  // later `carrick init` mints a new one, which is the reset the file exists
  // to give (carrick-cloud#890).
  if (plan.installId) {
    try {
      if (removeInstallId()) out.done("This machine's install id removed");
    } catch (error) {
      out.refuse(`${(error as Error).message}. Delete ${installIdPath()} by hand.`);
    }
  }

  if (plan.credential !== null) {
    try {
      if (removeCredential()) {
        out.done("Signed out: the saved credential is gone");
        out.say("Revoke the key itself at https://app.carrick.tools/account.");
      }
    } catch (error) {
      out.refuse((error as Error).message);
    }
  }
}

/** The line a run that was stopped at its question ends on. */
export const NOTHING_REMOVED = "Nothing was removed.";

/**
 * Print the list, then take the typed answer (carrick#1573).
 *
 * Null to go ahead; otherwise the code the run ends with, having deleted
 * nothing. The answer is compared exactly, case included, as the dashboard
 * compares the slug a project delete is confirmed with. `--confirm` answers
 * the question whether or not there is a terminal to ask it in.
 */
async function confirmed(ask: {
  command: string;
  heading: string;
  deletes: string[];
  leaves: string[];
  expected: string;
  question: string;
  given: string | null;
  interactive: boolean;
  out: InitOutput;
}): Promise<number | null> {
  const { out } = ask;
  out.say(listBlock(ask.heading, ask.deletes));
  for (const line of ask.leaves) out.warn(line);
  let answer = ask.given;
  if (answer === null) {
    if (!ask.interactive) {
      process.stderr.write(
        `carrick ${ask.command}: use --confirm ${typed(ask.expected)} to remove these without a terminal.\n`,
      );
      return 1;
    }
    try {
      answer = await out.ask(ask.question);
    } catch (error) {
      if (!(error instanceof PromptCancelled)) throw error;
      out.refuse(`Cancelled. ${NOTHING_REMOVED}`);
      return 1;
    }
  }
  if (answer !== ask.expected) {
    out.refuse(`"${answer}" is not ${ask.expected}. ${NOTHING_REMOVED}`);
    return 1;
  }
  return null;
}

/** A usage answer: the help text exits 0, anything else it had to say exits 2. */
function usage(text: string, help: string): number {
  process.stdout.write(`${text}\n`);
  return text.startsWith(help) ? 0 : 2;
}

export async function remove(argv: string[]): Promise<number> {
  return removeWith(argv, createOutput(), process.stdin.isTTY === true);
}

/**
 * `remove` with the terminal stated rather than sniffed, as `initWith` does,
 * so a test can be the terminal that answers the question.
 */
export async function removeWith(argv: string[], out: InitOutput, interactive: boolean): Promise<number> {
  const parsed = parseRemoveArgs(argv);
  if (typeof parsed === "string") return usage(parsed, "carrick remove");
  const { workspace } = parsed;
  if (!fs.existsSync(workspace)) {
    process.stderr.write(`carrick remove: ${workspace} is not a directory on this machine\n`);
    return 1;
  }

  const plan = folderPlan(workspace);
  const deletes = folderLines(plan);
  if (deletes.length === 0) {
    for (const line of plan.leaves) out.warn(line);
    out.say("Nothing left to remove in this folder.");
  } else {
    const name = folderName(workspace);
    const stop = await confirmed({
      command: "remove",
      heading: `Remove from ${tilde(workspace)}:`,
      deletes,
      leaves: plan.leaves,
      expected: name,
      question: `Type this folder's name, ${name}, to remove these`,
      given: parsed.confirm,
      interactive,
      out,
    });
    if (stop !== null) return stop;
    removeFolder(plan, out);
  }

  const leftovers = repoLeftovers(workspace);
  if (leftovers.files.length > 0 || leftovers.sections.length > 0) {
    out.say("");
    out.say("In the repository, which this command does not edit:");
    if (leftovers.files.length > 0) out.say(`  git rm ${leftovers.files.join(" ")}`);
    for (const section of leftovers.sections) out.say(`  by hand — ${section}`);
  }
  out.say("The MCP server, the install id and the sign-in belong to this machine and stay: carrick uninstall removes them.");
  return 0;
}

export async function uninstall(argv: string[]): Promise<number> {
  return uninstallWith(argv, createOutput(), process.stdin.isTTY === true);
}

/** `uninstall` with the terminal stated, as `removeWith`. */
export async function uninstallWith(argv: string[], out: InitOutput, interactive: boolean): Promise<number> {
  const parsed = parseUninstallArgs(argv);
  if (typeof parsed === "string") return usage(parsed, "carrick uninstall");

  const plan = machinePlan(parsed.keepLogin);
  const deletes = machineLines(plan);
  if (deletes.length === 0) {
    for (const line of plan.leaves) out.warn(line);
    out.say("Nothing left to remove on this machine.");
  } else {
    const stop = await confirmed({
      command: "uninstall",
      heading: "Remove from this machine:",
      deletes,
      leaves: plan.leaves,
      expected: UNINSTALL_ANSWER,
      question: `Type ${UNINSTALL_ANSWER} to remove these`,
      given: parsed.confirm,
      interactive,
      out,
    });
    if (stop !== null) return stop;
    removeMachine(plan, out);
  }

  if (parsed.keepLogin) out.say(`This machine stays signed in; the credential is ${tilde(credentialPath())}.`);
  out.note("Last steps, when nothing else on this machine needs it", [
    "carrick remove, in each folder carrick init set up",
    "npm uninstall -g carrick",
  ]);
  return 0;
}
