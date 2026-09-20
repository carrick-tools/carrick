// Connecting this machine's agent clients to the workspace MCP server.
//
// `carrick init` used to print one line for one client and leave the rest to
// the reader. It now configures every client it can see on this machine, and
// prints the line only for what it could not (carrick#955). One connection
// serves every project in the workspace, so this is a once-per-machine step.
//
// Two ways to configure a client, and the choice is not stylistic:
//
// 1. **Its own command**, where the client ships one. Claude Code's
//    `claude mcp add` is the line this CLI already printed, so running it is
//    the same act the reader was being asked to perform, and the file layout
//    stays the client's business.
// 2. **Its config file**, for clients with no such command. The shape of an
//    entry is read off the file wherever the file can state it — the
//    container key it already uses, and the URL key its existing entries use
//    — so extending a configured client follows that client rather than a
//    remembered schema. Only a file that does not exist yet is written from
//    the default below, and every path written is printed so a wrong guess is
//    one visible line and one entry to delete.
//
// A client is "on this machine" when its own data directory exists; those
// directories are created by the client, never by us. Nothing here may throw:
// a client that cannot be configured falls back to a printed line, because a
// failed MCP write must not fail a setup that has already written files.
//
// The other half of each writer is here too, because `carrick remove` has to
// undo exactly what was done and by the same rules (carrick#1034): the same
// detection, the same container the file uses, and one gate of its own — an
// entry is only removed when it points at this server's host.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

import { INSTALL_ID_HEADER, INSTALL_ID_PATTERN, installIdOrNull, readInstallId } from "./install-id.ts";

export const MCP_URL = "https://api.carrick.tools/mcp";
export const MCP_NAME = "carrick";
/** The host an entry has to name before `carrick remove` will take it out. */
export const MCP_HOST = new URL(MCP_URL).hostname;

/**
 * The arguments that add this server to Claude Code, with the install id on it.
 *
 * `--header` goes LAST, after the name and the URL, and that is not a style
 * choice: the flag is variadic (`-H, --header <header...>`), so a `--header`
 * placed before the positionals swallows them and `claude mcp add` exits with
 * "missing required argument 'name'". Verified against Claude Code 2.1.272.
 */
export function claudeAddArgs(installId: string | null): string[] {
  const args = ["mcp", "add", "--scope", "user", "--transport", "http", MCP_NAME, MCP_URL];
  return installId === null ? args : [...args, "--header", `${INSTALL_ID_HEADER}: ${installId}`];
}

/** The same thing as a line to paste, for the places that print one. */
export function mcpLine(installId: string | null = installIdOrNull()): string {
  const base = `claude mcp add --scope user --transport http ${MCP_NAME} ${MCP_URL}`;
  return installId === null ? base : `${base} --header "${INSTALL_ID_HEADER}: ${installId}"`;
}

// Why nothing here ever puts the install id on an entry that already exists.
//
// The id is one field on the server's own log line, beside the user agent
// (`lambdas/mcp-server/src/lambda.ts`): it authorises nothing, gates nothing,
// and an absent header logs as null. Nothing a user sees depends on it.
//
// Adding it to a working entry costs that user their sign-in. Claude Code keys
// its stored OAuth record on `name|sha256({type,url,headers})` — headers
// included — so an entry that grows a header is an entry whose token is filed
// under a key nothing looks up any more, and the next session goes back through
// the server's OAuth. That holds however the header gets there: `claude mcp
// remove && claude mcp add` is the same act with the cost made visible, which
// is why this command neither runs it nor prints it (carrick#1365).
//
// So an entry this command CREATES carries the id, an entry that is already
// there is left exactly as it is, and nothing is said about the difference.

/** Everything this module touches outside itself, so a test can state a machine. */
export type McpEnvironment = {
  onPath: (command: string) => boolean;
  /** Exit status of a command, or null when it could not be started. */
  run: (command: string, args: string[]) => number | null;
  /** A command's exit status and what it printed, for a read we have to parse. */
  capture: (command: string, args: string[]) => { status: number | null; stdout: string };
  home: string;
  platform: NodeJS.Platform;
  /**
   * This machine's install id, minting one if it has none: the writers' one.
   *
   * A function, not a value, so the file is only ever created by a call that
   * is about to configure a client.
   */
  installId: () => string | null;
  /**
   * The id this machine already has, never creating one: the reader's.
   *
   * `inspectMcpClients` is what `carrick doctor` runs, and that command writes
   * nothing at all — not even a file of ours in a directory of ours.
   */
  storedInstallId: () => string | null;
  appData: string | null;
  readFile: (file: string) => string | null;
  writeFile: (file: string, body: string) => void;
  exists: (target: string) => boolean;
};

export function realEnvironment(): McpEnvironment {
  return {
    onPath: (command) =>
      spawnSync(process.platform === "win32" ? "where" : "which", [command], {
        stdio: "ignore",
      }).status === 0,
    // Time-limited and with no stdin: a client's own command is someone
    // else's code, and `init` has already written files by the time it runs.
    // A command that hangs or asks a question reads as one that did not
    // configure anything, and the line to run by hand is printed instead.
    run: (command, args) =>
      spawnSync(command, args, { stdio: "ignore", timeout: 15_000 }).status,
    capture: (command, args) => {
      const result = spawnSync(command, args, { encoding: "utf8", timeout: 15_000 });
      return { status: result.status, stdout: result.stdout ?? "" };
    },
    home: os.homedir(),
    platform: process.platform,
    installId: () => installIdOrNull(),
    storedInstallId: () => readInstallId(),
    appData: process.env["APPDATA"] ?? null,
    readFile: (file) => {
      try {
        return fs.readFileSync(file, "utf8");
      } catch {
        return null;
      }
    },
    writeFile: (file, body) => {
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, body);
    },
    exists: (target) => fs.existsSync(target),
  };
}

/** A client configured by writing a file it owns. */
export type FileClient = {
  name: string;
  /** The client's own data directory: its existence is the detection. */
  directory: (env: McpEnvironment) => string | null;
  file: (env: McpEnvironment) => string | null;
  /** Container key for a file this client has not written yet. */
  container: "mcpServers" | "servers";
  /** URL key for a file this client has not written yet. */
  urlKey: "url" | "serverUrl";
  /** Whether a fresh entry states `"type": "http"`. */
  statesType: boolean;
  /**
   * Whether this client reads a `headers` object on an HTTP server entry.
   *
   * Only a client whose format is known gets the install id: a header written
   * into a shape a client does not read is a header that never arrives, and
   * one this command would then have to explain. Cursor, Windsurf and VS Code
   * all document `headers` for HTTP servers, so all three carry it.
   */
  headers: boolean;
};

function vsCodeUserDirectory(env: McpEnvironment): string | null {
  if (env.platform === "darwin") {
    return path.join(env.home, "Library", "Application Support", "Code", "User");
  }
  if (env.platform === "win32") {
    return env.appData ? path.join(env.appData, "Code", "User") : null;
  }
  return path.join(env.home, ".config", "Code", "User");
}

const FILE_CLIENTS: FileClient[] = [
  {
    name: "Cursor",
    directory: (env) => path.join(env.home, ".cursor"),
    file: (env) => path.join(env.home, ".cursor", "mcp.json"),
    container: "mcpServers",
    urlKey: "url",
    statesType: false,
    headers: true,
  },
  {
    name: "Windsurf",
    directory: (env) => path.join(env.home, ".codeium", "windsurf"),
    file: (env) => path.join(env.home, ".codeium", "windsurf", "mcp_config.json"),
    container: "mcpServers",
    urlKey: "serverUrl",
    statesType: false,
    headers: true,
  },
  {
    name: "VS Code",
    directory: vsCodeUserDirectory,
    file: (env) => {
      const directory = vsCodeUserDirectory(env);
      return directory === null ? null : path.join(directory, "mcp.json");
    },
    container: "servers",
    urlKey: "url",
    statesType: true,
    headers: true,
  },
];

/** What happened for one client, in the order init prints it. */
export type McpOutcome = {
  client: string;
  state: "written" | "present" | "failed";
  /** The line to print: the path written, or what to do by hand. */
  detail: string;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** The document back, with `servers` in the container it came from. */
function withServers(
  document: Record<string, unknown>,
  container: string,
  servers: Record<string, unknown>,
): string {
  const merged: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(document)) {
    merged[key] = key === container ? servers : value;
  }
  if (!(container in merged)) merged[container] = servers;
  return `${JSON.stringify(merged, null, 2)}\n`;
}

/**
 * The entry to add, and where, for one client's existing file.
 *
 * Exported for the test that states each shape: this is the half that reads a
 * file rather than remembering one.
 */
export function mergeServerEntry(
  client: FileClient,
  existing: string | null,
  installId: string | null = null,
): { body: string; state: "written" | "present" } {
  const document: Record<string, unknown> =
    existing === null || existing.trim() === "" ? {} : (JSON.parse(existing) as Record<string, unknown>);
  if (!isRecord(document)) throw new Error("the file is not a JSON object");

  // The container the file already uses wins over the default: a client that
  // renamed its key between versions has stated which one it reads.
  const container: "mcpServers" | "servers" = isRecord(document[client.container])
    ? client.container
    : isRecord(document["mcpServers"])
      ? "mcpServers"
      : isRecord(document["servers"])
        ? "servers"
        : client.container;
  const servers = isRecord(document[container]) ? { ...(document[container] as Record<string, unknown>) } : {};
  // Connected already, and that is the end of it: an entry that answers is not
  // edited to carry a header nobody asked for (see the note above
  // `mergeServerEntry`'s file header).
  if (MCP_NAME in servers) return { body: `${JSON.stringify(document, null, 2)}\n`, state: "present" };

  // The URL key and the type field are read off a sibling where there is one.
  const siblings = Object.values(servers).filter(isRecord);
  const urlKey = siblings.some((entry) => "serverUrl" in entry)
    ? "serverUrl"
    : siblings.some((entry) => "url" in entry)
      ? "url"
      : client.urlKey;
  const statesType = siblings.some((entry) => typeof entry["type"] === "string")
    ? true
    : siblings.length > 0
      ? false
      : client.statesType;
  const entry: Record<string, unknown> = statesType
    ? { type: "http", [urlKey]: MCP_URL }
    : { [urlKey]: MCP_URL };
  if (client.headers && installId !== null) entry["headers"] = { [INSTALL_ID_HEADER]: installId };

  servers[MCP_NAME] = entry;
  return { body: withServers(document, container, servers), state: "written" };
}

/**
 * Claude Code, through the command it ships.
 *
 * Detected the same way as the others — its own data directory — and not by
 * the command alone: a machine where `claude` resolves but nothing has ever
 * run it has no user configuration to add a server to, and the detection rule
 * stays one rule.
 */
function configureClaudeCode(env: McpEnvironment): McpOutcome | null {
  if (!env.onPath("claude") || !env.exists(path.join(env.home, ".claude"))) return null;
  const installId = env.installId();
  // `claude mcp add` refuses a name it already holds, so the read comes first.
  // It is read rather than tested because it also prints the headers on the
  // entry, which is the only way to tell an entry that predates the install id
  // from one that carries it.
  const read = env.capture("claude", ["mcp", "get", MCP_NAME]);
  if (read.status === 0) {
    return { client: "Claude Code", state: "present", detail: `already connected as "${MCP_NAME}"` };
  }
  const status = env.run("claude", claudeAddArgs(installId));
  return status === 0
    ? { client: "Claude Code", state: "written", detail: "connected for this user" }
    : { client: "Claude Code", state: "failed", detail: mcpLine(installId) };
}

function configureFileClient(env: McpEnvironment, client: FileClient): McpOutcome | null {
  const directory = client.directory(env);
  const file = client.file(env);
  if (directory === null || file === null) return null;
  if (!env.exists(directory) && !env.exists(file)) return null;
  const existing = env.readFile(file);
  try {
    const { body, state } = mergeServerEntry(client, existing, env.installId());
    if (state === "present") {
      return { client: client.name, state: "present", detail: `already in ${file}` };
    }
    env.writeFile(file, body);
    return { client: client.name, state: "written", detail: file };
  } catch {
    // A hand-edited file that no longer parses is a thing to report, never a
    // thing to replace.
    return {
      client: client.name,
      state: "failed",
      detail: `${file} is not valid JSON. Add "${MCP_NAME}" (${MCP_URL}) there by hand.`,
    };
  }
}

/**
 * One editor this machine could be asked about: its file, and how sure we are.
 *
 * `installed` is a second, stronger signal than the one that decides whether
 * to offer at all. A configuration directory outlives the editor that made it
 * — an uninstall leaves `~/.cursor` where it stands — so a directory is enough
 * to say "you may want this" and not enough to tick the box for somebody
 * (carrick#1365). The editor's own command answering on PATH is.
 */
export type OfferedClient = {
  name: string;
  /** The file that would be written, absolute, for the question to name. */
  file: string;
  installed: boolean;
};

/** What each file client calls its command, for the stronger signal above. */
const CLIENT_COMMANDS: Record<string, string> = {
  Cursor: "cursor",
  Windsurf: "windsurf",
  "VS Code": "code",
};

/**
 * The editors this machine could have an entry written for, with their files.
 *
 * Everything outside the workspace that `carrick init` would touch is in this
 * list, so the question it is printed under names every file the answer covers.
 * An editor whose configuration directory is not there is not offered: it is
 * not on this machine, and a question about it is a question about nothing.
 */
export function offeredFileClients(env: McpEnvironment = realEnvironment()): OfferedClient[] {
  const offered: OfferedClient[] = [];
  for (const client of FILE_CLIENTS) {
    const directory = client.directory(env);
    const file = client.file(env);
    if (directory === null || file === null) continue;
    if (!env.exists(directory) && !env.exists(file)) continue;
    const command = CLIENT_COMMANDS[client.name];
    offered.push({ name: client.name, file, installed: command !== undefined && env.onPath(command) });
  }
  return offered;
}

/**
 * Configure Claude Code, and the editors that were named in the answer.
 *
 * `editors` is the set of [`offeredFileClients`] names this run may write for.
 * It is a parameter and not a detection because the detection is what the
 * question was asked about: `~/.cursor/mcp.json` is somebody's file, outside
 * the workspace, for an editor they may not use, and it used to be written on
 * the strength of a yes to "Write the proposal and configure hooks?"
 * (carrick#1365).
 */
export function connectMcpClients(
  editors: readonly string[],
  env: McpEnvironment = realEnvironment(),
): McpOutcome[] {
  const outcomes: McpOutcome[] = [];
  const claude = configureClaudeCode(env);
  if (claude) outcomes.push(claude);
  for (const client of FILE_CLIENTS) {
    if (!editors.includes(client.name)) continue;
    const outcome = configureFileClient(env, client);
    if (outcome) outcomes.push(outcome);
  }
  return outcomes;
}

/** What happened for one client when `carrick remove` ran (carrick#1034). */
export type McpRemoval = {
  client: string;
  /**
   * `kept` is the one that matters: a server called `carrick` that names some
   * other host is somebody else's, and this command will not take it out.
   */
  state: "removed" | "absent" | "kept" | "failed";
  detail: string;
};

/** The URL an entry names, whichever key this client's file spells it with. */
function entryUrl(entry: Record<string, unknown>): string | null {
  const url = entry["url"] ?? entry["serverUrl"];
  return typeof url === "string" ? url : null;
}

/** Whether a URL points at the Carrick MCP server this package installs. */
function isCarrickUrl(url: string | null): boolean {
  if (url === null) return false;
  try {
    return new URL(url).hostname === MCP_HOST;
  } catch {
    return false;
  }
}

/**
 * The inverse of `mergeServerEntry`: our entry out of a client's own file.
 *
 * Both containers are searched, for the same reason the writer reads the one
 * the file already uses — the entry is wherever this client put it, not
 * wherever the default says. The removal is gated on the URL: a server called
 * `carrick` pointing at some other host was never written by `carrick init`,
 * and a command that removes it is a command nobody can run safely.
 *
 * A container this leaves empty goes with the entry, so a file init created
 * comes back out as the document it was merged into.
 */
export function removeServerEntry(existing: string | null): {
  body: string;
  state: "removed" | "absent" | "kept";
} {
  const source = existing === null || existing.trim() === "" ? "{}" : existing;
  const document: unknown = JSON.parse(source);
  if (!isRecord(document)) throw new Error("the file is not a JSON object");

  const container = (["mcpServers", "servers"] as const).find((key) => {
    const servers = document[key];
    return isRecord(servers) && MCP_NAME in servers;
  });
  if (container === undefined) return { body: source, state: "absent" };

  const servers = { ...(document[container] as Record<string, unknown>) };
  const entry = servers[MCP_NAME];
  if (!isRecord(entry) || !isCarrickUrl(entryUrl(entry))) {
    return { body: source, state: "kept" };
  }

  delete servers[MCP_NAME];
  const remaining: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(document)) {
    if (key === container) {
      // An empty container is a key that says nothing; the writer leaves no
      // empty group behind either (`settings.ts`).
      if (Object.keys(servers).length > 0) remaining[key] = servers;
      continue;
    }
    remaining[key] = value;
  }
  return { body: `${JSON.stringify(remaining, null, 2)}\n`, state: "removed" };
}

/**
 * Claude Code, through the command it ships.
 *
 * `mcp get` is read first for the same reason the writer reads it: it is the
 * only statement of what this machine actually holds, and its URL line is what
 * gates the removal. A server in another scope — a project's `.mcp.json`, a
 * local one — is not what `carrick init` wrote, so the removal names the user
 * scope and a failure prints the line that removes whatever is left.
 */
function disconnectClaudeCode(env: McpEnvironment): McpRemoval | null {
  if (!env.onPath("claude") || !env.exists(path.join(env.home, ".claude"))) return null;
  const read = env.capture("claude", ["mcp", "get", MCP_NAME]);
  if (read.status !== 0) return { client: "Claude Code", state: "absent", detail: "no carrick server" };
  if (!read.stdout.includes(MCP_HOST)) {
    return {
      client: "Claude Code",
      state: "kept",
      detail: `"${MCP_NAME}" there does not point at ${MCP_HOST}, so it was left alone`,
    };
  }
  const status = env.run("claude", ["mcp", "remove", "--scope", "user", MCP_NAME]);
  return status === 0
    ? { client: "Claude Code", state: "removed", detail: "MCP server removed for this user" }
    : {
        client: "Claude Code",
        state: "failed",
        detail: `claude mcp remove ${MCP_NAME}`,
      };
}

function disconnectFileClient(env: McpEnvironment, client: FileClient): McpRemoval | null {
  const directory = client.directory(env);
  const file = client.file(env);
  if (directory === null || file === null) return null;
  if (!env.exists(directory) && !env.exists(file)) return null;
  const existing = env.readFile(file);
  if (existing === null) return null;
  try {
    const { body, state } = removeServerEntry(existing);
    if (state === "absent") return { client: client.name, state, detail: `no carrick server in ${file}` };
    if (state === "kept") {
      return {
        client: client.name,
        state,
        detail: `"${MCP_NAME}" in ${file} does not point at ${MCP_HOST}, so it was left alone`,
      };
    }
    env.writeFile(file, body);
    return { client: client.name, state: "removed", detail: file };
  } catch {
    return {
      client: client.name,
      state: "failed",
      detail: `${file} is not valid JSON. Remove "${MCP_NAME}" there by hand.`,
    };
  }
}

/** What one client's configuration says about our server (carrick#1035). */
export type McpInspection = {
  client: string;
  /**
   * `connected` — an entry naming this server's host. Whether it carries the
   * install id is not part of it: that header is a log field, an entry without
   * one answers every question the same way, and putting one on would cost the
   * sign-in (see the note above `mergeServerEntry`, carrick#1365).
   * `elsewhere` — an entry called `carrick` pointing somewhere else, which is
   * somebody else's and the reason a reader gets no Carrick answers in that
   * client. `absent` — this client is on the machine and has no entry.
   * `unreadable` — its file is not JSON, or its own command would not answer.
   */
  state: "connected" | "absent" | "elsewhere" | "unreadable";
  detail: string;
};

/**
 * What each client on this machine holds, changing nothing.
 *
 * The third half of the pair in this file: `connectMcpClients` writes,
 * `disconnectMcpClients` removes, and this one only reads. Same detection (the
 * client's own data directory), same containers, same URL gate — an entry is
 * ours when it names `MCP_HOST`, which is what makes "connected" mean
 * connected to Carrick rather than to a server that borrowed the name.
 */
export function inspectMcpClients(env: McpEnvironment = realEnvironment()): McpInspection[] {
  const found: McpInspection[] = [];
  if (env.onPath("claude") && env.exists(path.join(env.home, ".claude"))) {
    const read = env.capture("claude", ["mcp", "get", MCP_NAME]);
    if (read.status === null) {
      found.push({ client: "Claude Code", state: "unreadable", detail: "`claude mcp get carrick` did not answer" });
    } else if (read.status !== 0) {
      found.push({ client: "Claude Code", state: "absent", detail: `no "${MCP_NAME}" server for this user` });
    } else if (!read.stdout.includes(MCP_HOST)) {
      found.push({
        client: "Claude Code",
        state: "elsewhere",
        detail: `"${MCP_NAME}" there does not point at ${MCP_HOST}`,
      });
    } else {
      found.push({ client: "Claude Code", state: "connected", detail: MCP_URL });
    }
  }
  for (const client of FILE_CLIENTS) {
    const directory = client.directory(env);
    const file = client.file(env);
    if (directory === null || file === null) continue;
    if (!env.exists(directory) && !env.exists(file)) continue;
    const existing = env.readFile(file);
    if (existing === null) {
      found.push({ client: client.name, state: "absent", detail: `no MCP configuration at ${file}` });
      continue;
    }
    let document: unknown;
    try {
      document = existing.trim() === "" ? {} : JSON.parse(existing);
    } catch {
      found.push({ client: client.name, state: "unreadable", detail: `${file} is not valid JSON` });
      continue;
    }
    if (!isRecord(document)) {
      found.push({ client: client.name, state: "unreadable", detail: `${file} is not a JSON object` });
      continue;
    }
    const container = (["mcpServers", "servers"] as const).find((key) => {
      const servers = document[key];
      return isRecord(servers) && MCP_NAME in servers;
    });
    if (container === undefined) {
      found.push({ client: client.name, state: "absent", detail: `no "${MCP_NAME}" server in ${file}` });
      continue;
    }
    const entry = (document[container] as Record<string, unknown>)[MCP_NAME];
    const url = isRecord(entry) ? entryUrl(entry) : null;
    if (!isCarrickUrl(url)) {
      found.push({
        client: client.name,
        state: "elsewhere",
        detail: `"${MCP_NAME}" in ${file} points at ${url ?? "no URL"}`,
      });
      continue;
    }
    found.push({ client: client.name, state: "connected", detail: url ?? MCP_URL });
  }
  return found;
}

/** Take our server out of every client this machine has. */
export function disconnectMcpClients(env: McpEnvironment = realEnvironment()): McpRemoval[] {
  const removals: McpRemoval[] = [];
  const claude = disconnectClaudeCode(env);
  if (claude) removals.push(claude);
  for (const client of FILE_CLIENTS) {
    const removal = disconnectFileClient(env, client);
    if (removal) removals.push(removal);
  }
  return removals;
}
