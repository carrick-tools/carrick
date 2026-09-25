// Connecting the agent clients on a machine to the workspace MCP server
// (carrick#955).
//
// Every test states a machine rather than using this one: the subject is a
// user's own configuration files, and a test that reached the real home
// directory would be editing them.

import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import {
  claudeAddArgs,
  connectMcpClients,
  offeredFileClients,
  disconnectMcpClients,
  inspectMcpClients,
  mcpLine,
  mergeServerEntry,
  removeServerEntry,
  MCP_URL,
  type McpEnvironment,
  type FileClient,
} from "../src/init/mcp.ts";
import { INSTALL_ID_HEADER } from "../src/init/install-id.ts";

const HOME = "/home/dev";
/** This machine's id, stated rather than generated: the file is `install-id.test.ts`'s subject. */
const INSTALL_ID = "11111111-2222-4333-8444-555555555555";
/** The add, as the `ran` log of a fake spawn joins it: one argv entry per word. */
const ADD_LINE = `claude mcp add --scope user --transport http carrick ${MCP_URL} --header ${INSTALL_ID_HEADER}: ${INSTALL_ID}`;
/** The same add as a line a person pastes, where the header needs its quotes. */
const ADD_LINE_QUOTED = `claude mcp add --scope user --transport http carrick ${MCP_URL} --header "${INSTALL_ID_HEADER}: ${INSTALL_ID}"`;

type Machine = {
  commands?: string[];
  directories?: string[];
  files?: Record<string, string>;
  /** Exit status per command line, keyed by `command args...`. */
  statuses?: Record<string, number>;
  /** What a command prints, keyed the same way, for the reads that parse it. */
  outputs?: Record<string, string>;
  /** This machine's install id; null is a machine that could not make one. */
  installId?: string | null;
};

function machine(state: Machine): { env: McpEnvironment; written: Record<string, string>; ran: string[] } {
  const files = { ...(state.files ?? {}) };
  const written: Record<string, string> = {};
  const ran: string[] = [];
  const env: McpEnvironment = {
    onPath: (command) => (state.commands ?? []).includes(command),
    run: (command, args) => {
      const line = [command, ...args].join(" ");
      ran.push(line);
      return state.statuses?.[line] ?? 0;
    },
    capture: (command, args) => {
      const line = [command, ...args].join(" ");
      ran.push(line);
      return { status: state.statuses?.[line] ?? 0, stdout: state.outputs?.[line] ?? "" };
    },
    home: HOME,
    platform: "linux",
    appData: null,
    installId: () => (state.installId === undefined ? INSTALL_ID : state.installId),
    storedInstallId: () => (state.installId === undefined ? INSTALL_ID : state.installId),
    readFile: (file) => files[file] ?? null,
    writeFile: (file, body) => {
      files[file] = body;
      written[file] = body;
    },
    exists: (target) => (state.directories ?? []).includes(target) || target in files,
  };
  return { env, written, ran };
}

const cursorFile = path.join(HOME, ".cursor", "mcp.json");
const windsurfFile = path.join(HOME, ".codeium", "windsurf", "mcp_config.json");
const vsCodeFile = path.join(HOME, ".config", "Code", "User", "mcp.json");

const cursor: FileClient = {
  name: "Cursor",
  directory: (env) => path.join(env.home, ".cursor"),
  file: (env) => path.join(env.home, ".cursor", "mcp.json"),
  container: "mcpServers",
  urlKey: "url",
  statesType: false,
  headers: true,
};

/** What `claude mcp get carrick` prints for an entry this release wrote. */
const CLAUDE_GET = `carrick:
  Scope: User config (available in all your projects)
  Type: http
  URL: ${MCP_URL}
  Headers:
    ${INSTALL_ID_HEADER}: ${INSTALL_ID}
`;

/** Every editor this machine could be asked about, for the writers' tests. */
const EVERY_EDITOR = ["Cursor", "Windsurf", "VS Code"];

/** The same entry as written before the install id existed. */
const UNSTAMPED_GET = `carrick:
  Scope: User config (available in all your projects)
  Type: http
  URL: ${MCP_URL}
`;

test("a machine with no claude command is told the line, and nothing is written", () => {
  const { env, written, ran } = machine({ directories: [path.join(HOME, ".claude")] });
  assert.deepEqual(connectMcpClients(EVERY_EDITOR, env), []);
  assert.deepEqual(written, {});
  assert.deepEqual(ran, []);
});

// carrick#1489. `claude mcp add` creates the configuration it writes to, so a
// Claude Code that has never been run is still one this command can set up.
// Requiring its directory made init write its hooks and then report "no agent
// client found".
test("Claude Code is connected by its command even before it has ever run", () => {
  const { env, ran } = machine({ commands: ["claude"], statuses: { "claude mcp get carrick": 1 } });
  assert.deepEqual(connectMcpClients([], env), [
    { client: "Claude Code", state: "written", detail: "connected for this user" },
  ]);
  assert.deepEqual(ran, ["claude mcp get carrick", ADD_LINE]);
});

test("Claude Code is connected by its own command, once", () => {
  const { env, ran } = machine({
    commands: ["claude"],
    directories: [path.join(HOME, ".claude")],
    statuses: { "claude mcp get carrick": 1 },
  });
  const outcomes = connectMcpClients(EVERY_EDITOR, env);
  assert.deepEqual(outcomes, [
    { client: "Claude Code", state: "written", detail: "connected for this user" },
  ]);
  // The header goes on the end, after the name and the URL: `--header` is
  // variadic, so a flag placed before them swallows both and the command
  // exits with "missing required argument 'name'" (Claude Code 2.1.272).
  assert.deepEqual(ran, ["claude mcp get carrick", ADD_LINE]);
  assert.deepEqual(claudeAddArgs(INSTALL_ID).slice(-2), [
    "--header",
    `${INSTALL_ID_HEADER}: ${INSTALL_ID}`,
  ]);
  assert.equal(mcpLine(INSTALL_ID), ADD_LINE_QUOTED);

  // A client that already holds the server is left exactly as it is: `mcp add`
  // refuses a name it already has, so the read is what decides.
  const second = machine({
    commands: ["claude"],
    directories: [path.join(HOME, ".claude")],
    statuses: { "claude mcp get carrick": 0 },
    outputs: { "claude mcp get carrick": CLAUDE_GET },
  });
  assert.equal(connectMcpClients(EVERY_EDITOR, second.env)[0]?.state, "present");
  assert.deepEqual(second.ran, ["claude mcp get carrick"]);
});

test("a command that fails leaves the line to copy, and no claim", () => {
  const { env } = machine({
    commands: ["claude"],
    directories: [path.join(HOME, ".claude")],
    statuses: { "claude mcp get carrick": 1, [ADD_LINE]: 2 },
  });
  const outcomes = connectMcpClients(EVERY_EDITOR, env);
  assert.equal(outcomes[0]?.state, "failed");
  assert.equal(outcomes[0]?.detail, mcpLine(INSTALL_ID));
});

// An entry that already answers is left exactly as it is, and nothing is said
// about it. The install id is a field on the server's own log line and nothing
// a user sees depends on it; putting one on an entry that has none costs that
// user their sign-in, because Claude Code keys its stored OAuth record on
// `name|sha256({type,url,headers})` — headers included (carrick#1365).
test("a Claude Code entry with no install id is left alone, and not mentioned", () => {
  const { env, ran } = machine({
    commands: ["claude"],
    directories: [path.join(HOME, ".claude")],
    outputs: { "claude mcp get carrick": UNSTAMPED_GET },
  });
  assert.deepEqual(connectMcpClients(EVERY_EDITOR, env), [
    { client: "Claude Code", state: "present", detail: 'already connected as "carrick"' },
  ]);
  // Read, and nothing else: `carrick init` runs no `mcp remove`, ever.
  assert.deepEqual(ran, ["claude mcp get carrick"]);

  // The entry that already carries one is read and left alone too, and the
  // two are reported identically: there is nothing to tell apart.
  const stamped = machine({
    commands: ["claude"],
    directories: [path.join(HOME, ".claude")],
    outputs: { "claude mcp get carrick": CLAUDE_GET },
  });
  assert.equal(connectMcpClients(EVERY_EDITOR, stamped.env)[0]?.state, "present");
  assert.deepEqual(stamped.ran, ["claude mcp get carrick"]);

  // And a `carrick` pointing at somebody else's server is not re-written to
  // carry our id, whatever it is called.
  const elsewhere = machine({
    commands: ["claude"],
    directories: [path.join(HOME, ".claude")],
    outputs: { "claude mcp get carrick": "carrick:\n  URL: https://mcp.example.test/mcp\n" },
  });
  assert.equal(connectMcpClients(EVERY_EDITOR, elsewhere.env)[0]?.state, "present");
  assert.deepEqual(elsewhere.ran, ["claude mcp get carrick"]);
});

// The same for a file client: an entry that is there is not edited to grow a
// header, so a re-run writes nothing at all.
test("an editor entry with no install id is not rewritten", () => {
  const cursor = path.join(HOME, ".cursor", "mcp.json");
  const { env, written } = machine({
    directories: [path.join(HOME, ".cursor")],
    files: { [cursor]: JSON.stringify({ mcpServers: { carrick: { url: MCP_URL } } }, null, 2) },
  });
  assert.deepEqual(connectMcpClients(EVERY_EDITOR, env), [
    { client: "Cursor", state: "present", detail: `already in ${cursor}` },
  ]);
  assert.deepEqual(written, {});
});

// Which editors are offered, and which start ticked. A configuration
// directory outlives the editor that made it, so it is enough to ask about and
// not enough to answer for somebody (carrick#1365).
test("an editor is offered for its directory and ticked for its command", () => {
  const { env } = machine({
    commands: ["cursor"],
    directories: [path.join(HOME, ".cursor"), path.join(HOME, ".codeium", "windsurf")],
  });
  assert.deepEqual(offeredFileClients(env), [
    { name: "Cursor", file: cursorFile, installed: true },
    { name: "Windsurf", file: windsurfFile, installed: false },
  ]);
  // No directory, no row: VS Code is not on this machine and is not asked about.
  assert.deepEqual(
    offeredFileClients(machine({ commands: ["code"] }).env),
    [],
  );
});

// The answer is what decides, not the detection: an editor left out of it gets
// no file, however plainly its directory is there.
test("an editor left out of the answer is not written for", () => {
  const { env, written } = machine({
    commands: ["cursor", "windsurf"],
    directories: [path.join(HOME, ".cursor"), path.join(HOME, ".codeium", "windsurf")],
  });
  const outcomes = connectMcpClients(["Windsurf"], env);
  assert.deepEqual(
    outcomes.map((outcome) => outcome.client),
    ["Windsurf"],
  );
  assert.deepEqual(Object.keys(written), [windsurfFile]);

  // And nothing at all when the answer is empty, which is what a run with no
  // terminal and no --mcp gives.
  const declined = machine({
    commands: ["cursor"],
    directories: [path.join(HOME, ".cursor")],
  });
  assert.deepEqual(connectMcpClients([], declined.env), []);
  assert.deepEqual(declined.written, {});
});

// A home directory that will not take a file costs the header, never the
// setup: the client is still connected, and doctor reports the drift.
test("a machine that could not make an install id is still connected", () => {
  const { env, ran } = machine({
    commands: ["claude"],
    directories: [path.join(HOME, ".claude"), path.join(HOME, ".cursor")],
    statuses: { "claude mcp get carrick": 1 },
    installId: null,
  });
  const outcomes = connectMcpClients(EVERY_EDITOR, env);
  assert.deepEqual(
    outcomes.map((outcome) => outcome.state),
    ["written", "written"],
  );
  assert.deepEqual(ran, [
    "claude mcp get carrick",
    `claude mcp add --scope user --transport http carrick ${MCP_URL}`,
  ]);
  assert.deepEqual(JSON.parse(mergeServerEntry(cursor, null, null).body), {
    mcpServers: { carrick: { url: MCP_URL } },
  });
});

test("a client with no config file yet gets one in that client's own shape", () => {
  const { env, written } = machine({
    directories: [
      path.join(HOME, ".cursor"),
      path.join(HOME, ".codeium", "windsurf"),
      path.join(HOME, ".config", "Code", "User"),
    ],
  });
  const outcomes = connectMcpClients(EVERY_EDITOR, env);
  assert.deepEqual(
    outcomes.map((outcome) => outcome.client),
    ["Cursor", "Windsurf", "VS Code"],
  );
  // Every one of the three documents `headers` on an HTTP server, so every
  // one of them carries the install id (carrick-cloud#890).
  const headers = { [INSTALL_ID_HEADER]: INSTALL_ID };
  assert.deepEqual(JSON.parse(written[cursorFile]!), {
    mcpServers: { carrick: { url: MCP_URL, headers } },
  });
  assert.deepEqual(JSON.parse(written[windsurfFile]!), {
    mcpServers: { carrick: { serverUrl: MCP_URL, headers } },
  });
  assert.deepEqual(JSON.parse(written[vsCodeFile]!), {
    servers: { carrick: { type: "http", url: MCP_URL, headers } },
  });
  for (const outcome of outcomes) assert.equal(outcome.state, "written");
});

test("a client that is not on this machine is not configured", () => {
  const { env, written } = machine({ directories: [path.join(HOME, ".cursor")] });
  assert.deepEqual(
    connectMcpClients(EVERY_EDITOR, env).map((outcome) => outcome.client),
    ["Cursor"],
  );
  assert.deepEqual(Object.keys(written), [cursorFile]);
});

test("an existing file keeps every server it already holds", () => {
  const existing = JSON.stringify(
    { mcpServers: { other: { command: "uvx", args: ["some-server"] } }, somethingElse: 1 },
    null,
    2,
  );
  const merged = JSON.parse(mergeServerEntry(cursor, existing).body);
  assert.deepEqual(merged.mcpServers.other, { command: "uvx", args: ["some-server"] });
  assert.equal(merged.somethingElse, 1);
  assert.deepEqual(merged.mcpServers.carrick, { url: MCP_URL });
});

// The shape of an entry is read off the file wherever the file can state it,
// so a client that spells a remote server differently from the default is
// followed rather than contradicted.
test("the entry follows the keys the file already uses", () => {
  const serverUrlStyle = JSON.stringify({ mcpServers: { other: { serverUrl: "https://example.test/mcp" } } });
  assert.deepEqual(JSON.parse(mergeServerEntry(cursor, serverUrlStyle).body).mcpServers.carrick, {
    serverUrl: MCP_URL,
  });

  const typedStyle = JSON.stringify({ mcpServers: { other: { type: "http", url: "https://example.test/mcp" } } });
  assert.deepEqual(JSON.parse(mergeServerEntry(cursor, typedStyle).body).mcpServers.carrick, {
    type: "http",
    url: MCP_URL,
  });

  // A file that uses the other container key keeps using it.
  const otherContainer = JSON.stringify({ servers: { other: { url: "https://example.test/mcp" } } });
  const merged = JSON.parse(mergeServerEntry(cursor, otherContainer).body);
  assert.ok(merged.servers.carrick);
  assert.equal(merged.mcpServers, undefined);
});

test("a server already called carrick is left exactly as it is", () => {
  const existing = JSON.stringify({ mcpServers: { carrick: { url: "https://api.carrick.tools/mcp/p/one" } } }, null, 2);
  const result = mergeServerEntry(cursor, existing);
  assert.equal(result.state, "present");
  assert.deepEqual(JSON.parse(result.body), JSON.parse(existing));

  // And with an id in hand it is still left alone: an entry that answers is
  // not edited to carry a header, because the header is what the client's
  // stored OAuth record is keyed on (carrick#1365).
  const withId = mergeServerEntry(cursor, existing, INSTALL_ID);
  assert.equal(withId.state, "present");
  assert.deepEqual(JSON.parse(withId.body), JSON.parse(existing));

  const { env, written } = machine({ directories: [path.join(HOME, ".cursor")], files: { [cursorFile]: existing } });
  assert.equal(connectMcpClients(EVERY_EDITOR, env)[0]?.state, "present");
  assert.deepEqual(written, {});
});

// The id goes on the entry this command CREATES, with whatever else that
// entry needs, and a user's own headers on their own entry are never touched
// because their entry is never rewritten.
test("a new entry carries the install id, and a user's entry is not rewritten", () => {
  const fresh = mergeServerEntry(cursor, null, INSTALL_ID);
  assert.equal(fresh.state, "written");
  assert.deepEqual(JSON.parse(fresh.body).mcpServers.carrick, {
    url: MCP_URL,
    headers: { [INSTALL_ID_HEADER]: INSTALL_ID },
  });

  const theirs = JSON.stringify(
    {
      mcpServers: {
        other: { url: "https://example.test/mcp" },
        carrick: { type: "http", url: MCP_URL, headers: { Authorization: "Bearer theirs" } },
      },
    },
    null,
    2,
  );
  const kept = mergeServerEntry(cursor, theirs, INSTALL_ID);
  assert.equal(kept.state, "present");
  assert.deepEqual(JSON.parse(kept.body).mcpServers.carrick, {
    type: "http",
    url: MCP_URL,
    headers: { Authorization: "Bearer theirs" },
  });
});

test("a hand-edited file that no longer parses is reported, never replaced", () => {
  assert.throws(() => mergeServerEntry(cursor, "{ not json"));
  const { env, written } = machine({
    directories: [path.join(HOME, ".cursor")],
    files: { [cursorFile]: "{ not json" },
  });
  const outcome = connectMcpClients(EVERY_EDITOR, env)[0];
  assert.equal(outcome?.state, "failed");
  assert.match(outcome!.detail, /not valid JSON/);
  assert.deepEqual(written, {});
});

test("every path written is reported, for the line init prints", () => {
  const { env } = machine({ directories: [path.join(HOME, ".cursor")] });
  const outcomes = connectMcpClients(EVERY_EDITOR, env);
  assert.equal(outcomes[0]?.state, "written");
  // The detail IS the path, and `mcpClientLines` prints it: a guessed config
  // file has to be one visible line and one entry to delete.
  assert.equal(outcomes[0]?.detail, cursorFile);
});

// The other half: what `carrick remove` takes back out (carrick#1034). Each
// test states the machine the same way, because the subject is still a user's
// own configuration files.


test("what the writer added is exactly what the remover takes away", () => {
  // One document per client shape, each holding something of somebody else's,
  // so the round trip proves the file comes back as it was rather than as an
  // empty object.
  const documents = [
    JSON.stringify({ mcpServers: { other: { command: "uvx", args: ["some-server"] } }, somethingElse: 1 }, null, 2),
    JSON.stringify({ mcpServers: { other: { serverUrl: "https://example.test/mcp" } } }, null, 2),
    JSON.stringify({ servers: { other: { type: "http", url: "https://example.test/mcp" } } }, null, 2),
    "{}\n",
  ];
  for (const before of documents) {
    const written = mergeServerEntry(cursor, before, INSTALL_ID);
    assert.equal(written.state, "written");
    const removed = removeServerEntry(written.body);
    assert.equal(removed.state, "removed");
    assert.deepEqual(JSON.parse(removed.body), JSON.parse(before));
  }

  // And a file this package created from nothing comes back to nothing.
  const fresh = mergeServerEntry(cursor, null, INSTALL_ID).body;
  assert.deepEqual(JSON.parse(removeServerEntry(fresh).body), {});
});

test("a carrick entry that points somewhere else is kept, whatever it is called", () => {
  const elsewhere = JSON.stringify({ mcpServers: { carrick: { url: "https://mcp.example.test/mcp" } } }, null, 2);
  const kept = removeServerEntry(elsewhere);
  assert.equal(kept.state, "kept");
  assert.deepEqual(JSON.parse(kept.body), JSON.parse(elsewhere));

  // A stdio server named carrick states no URL at all, so it cannot be ours.
  const stdio = JSON.stringify({ mcpServers: { carrick: { command: "carrick-mcp" } } }, null, 2);
  assert.equal(removeServerEntry(stdio).state, "kept");

  const { env, written } = machine({
    directories: [path.join(HOME, ".cursor")],
    files: { [cursorFile]: elsewhere },
  });
  const removal = disconnectMcpClients(env)[0];
  assert.equal(removal?.state, "kept");
  assert.match(removal!.detail, /does not point at api\.carrick\.tools/);
  assert.deepEqual(written, {});
});

test("a file with no carrick server is left byte for byte", () => {
  const existing = JSON.stringify({ mcpServers: { other: { url: "https://example.test/mcp" } } }, null, 2);
  const outcome = removeServerEntry(existing);
  assert.equal(outcome.state, "absent");
  assert.equal(outcome.body, existing);

  const { env, written } = machine({
    directories: [path.join(HOME, ".cursor")],
    files: { [cursorFile]: existing },
  });
  assert.equal(disconnectMcpClients(env)[0]?.state, "absent");
  assert.deepEqual(written, {});
});

test("Claude Code is disconnected by its own command, and only on the URL it prints", () => {
  const { env, ran } = machine({
    commands: ["claude"],
    directories: [path.join(HOME, ".claude")],
    outputs: { "claude mcp get carrick": CLAUDE_GET },
  });
  assert.deepEqual(disconnectMcpClients(env), [
    { client: "Claude Code", state: "removed", detail: "MCP server removed for this user" },
  ]);
  assert.deepEqual(ran, ["claude mcp get carrick", "claude mcp remove --scope user carrick"]);

  // A server of that name pointing elsewhere is not ours to remove, and the
  // read is the only thing that can say so.
  const other = machine({
    commands: ["claude"],
    directories: [path.join(HOME, ".claude")],
    outputs: { "claude mcp get carrick": "carrick:\n  URL: https://mcp.example.test/mcp\n" },
  });
  assert.equal(disconnectMcpClients(other.env)[0]?.state, "kept");
  assert.deepEqual(other.ran, ["claude mcp get carrick"]);

  // Nothing configured: the read fails, and nothing else runs.
  const none = machine({
    commands: ["claude"],
    directories: [path.join(HOME, ".claude")],
    statuses: { "claude mcp get carrick": 1 },
  });
  assert.equal(disconnectMcpClients(none.env)[0]?.state, "absent");
  assert.deepEqual(none.ran, ["claude mcp get carrick"]);
});

test("a removal command that fails prints the line that finishes it", () => {
  const { env } = machine({
    commands: ["claude"],
    directories: [path.join(HOME, ".claude")],
    outputs: { "claude mcp get carrick": CLAUDE_GET },
    statuses: { "claude mcp remove --scope user carrick": 1 },
  });
  const removal = disconnectMcpClients(env)[0];
  assert.equal(removal?.state, "failed");
  assert.equal(removal?.detail, "claude mcp remove carrick");
});

test("removal touches only the clients this machine has, and reports each file", () => {
  const entry = (key: "url" | "serverUrl"): string =>
    JSON.stringify({ mcpServers: { carrick: { [key]: MCP_URL } } }, null, 2);
  const { env, written } = machine({
    directories: [path.join(HOME, ".cursor"), path.join(HOME, ".codeium", "windsurf")],
    files: { [cursorFile]: entry("url"), [windsurfFile]: entry("serverUrl") },
  });
  const removals = disconnectMcpClients(env);
  assert.deepEqual(
    removals.map((removal) => [removal.client, removal.state, removal.detail]),
    [
      ["Cursor", "removed", cursorFile],
      ["Windsurf", "removed", windsurfFile],
    ],
  );
  assert.deepEqual(Object.keys(written).sort(), [cursorFile, windsurfFile].sort());
  for (const body of Object.values(written)) assert.deepEqual(JSON.parse(body), {});
});

test("a hand-edited file that no longer parses is reported by the remover too", () => {
  assert.throws(() => removeServerEntry("{ not json"));
  const { env, written } = machine({
    directories: [path.join(HOME, ".cursor")],
    files: { [cursorFile]: "{ not json" },
  });
  const removal = disconnectMcpClients(env)[0];
  assert.equal(removal?.state, "failed");
  assert.match(removal!.detail, /not valid JSON/);
  assert.deepEqual(written, {});
});

test("the read-only inspection states each client, and writes nothing", () => {
  // Read-only is the whole point: `carrick doctor` runs this on a machine
  // whose owner already suspects something, and a check that repaired what it
  // found would be a check nobody could trust the output of (carrick#1035).
  const { env, written, ran } = machine({
    commands: ["claude"],
    directories: [path.join(HOME, ".claude"), path.join(HOME, ".cursor"), path.join(HOME, ".codeium", "windsurf")],
    files: {
      [cursorFile]: JSON.stringify({
        mcpServers: { carrick: { url: MCP_URL, headers: { [INSTALL_ID_HEADER]: INSTALL_ID } } },
      }),
      [windsurfFile]: JSON.stringify({ mcpServers: { carrick: { serverUrl: "http://localhost:9000/mcp" } } }),
    },
    outputs: { "claude mcp get carrick": CLAUDE_GET },
  });
  assert.deepEqual(
    inspectMcpClients(env).map((client) => [client.client, client.state]),
    [
      ["Claude Code", "connected"],
      ["Cursor", "connected"],
      ["Windsurf", "elsewhere"],
    ],
  );
  assert.deepEqual(written, {});
  assert.deepEqual(ran, ["claude mcp get carrick"]);
});

// An entry with no install id is a healthy connection. The id is a field on
// the server's own log line, an entry without one answers every question the
// same way, and adding one costs the sign-in (carrick#1365), so there is
// nothing here for `carrick doctor` to report.
test("an entry with no install id is a healthy connection", () => {
  const { env, written } = machine({
    commands: ["claude"],
    directories: [path.join(HOME, ".claude"), path.join(HOME, ".cursor"), path.join(HOME, ".codeium", "windsurf")],
    files: {
      [cursorFile]: JSON.stringify({ mcpServers: { carrick: { url: MCP_URL } } }),
      [windsurfFile]: JSON.stringify({
        mcpServers: { carrick: { serverUrl: MCP_URL, headers: { [INSTALL_ID_HEADER]: "short" } } },
      }),
    },
    outputs: { "claude mcp get carrick": UNSTAMPED_GET },
  });
  assert.deepEqual(
    inspectMcpClients(env).map((client) => [client.client, client.state]),
    [
      ["Claude Code", "connected"],
      ["Cursor", "connected"],
      ["Windsurf", "connected"],
    ],
  );
  assert.deepEqual(written, {});
});

test("a client with no entry, one whose file will not parse, and one this machine does not have", () => {
  const { env } = machine({
    commands: ["claude"],
    directories: [path.join(HOME, ".claude"), path.join(HOME, ".cursor")],
    files: { [cursorFile]: "{ not json" },
    statuses: { "claude mcp get carrick": 1 },
  });
  assert.deepEqual(
    inspectMcpClients(env).map((client) => [client.client, client.state]),
    [
      ["Claude Code", "absent"],
      ["Cursor", "unreadable"],
    ],
  );
});
