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
  connectMcpClients,
  mcpLines,
  mergeServerEntry,
  MCP_LINE,
  MCP_URL,
  type McpEnvironment,
  type FileClient,
} from "../src/init/mcp.ts";

const HOME = "/home/dev";

type Machine = {
  commands?: string[];
  directories?: string[];
  files?: Record<string, string>;
  /** Exit status per command line, keyed by `command args...`. */
  statuses?: Record<string, number>;
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
    home: HOME,
    platform: "linux",
    appData: null,
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

test("a machine with no agent client is told the line, and nothing is written", () => {
  const { env, written } = machine({ commands: ["claude"] });
  assert.deepEqual(connectMcpClients(env), []);
  assert.deepEqual(written, {});
  const lines = mcpLines([]);
  assert.ok(lines.some((line) => line.includes(MCP_LINE)));
  assert.ok(lines.some((line) => line.includes(MCP_URL)));
});

test("Claude Code is connected by its own command, once", () => {
  const { env, ran } = machine({
    commands: ["claude"],
    directories: [path.join(HOME, ".claude")],
    statuses: { "claude mcp get carrick": 1 },
  });
  const outcomes = connectMcpClients(env);
  assert.deepEqual(outcomes, [
    { client: "Claude Code", state: "written", detail: "connected for this user" },
  ]);
  assert.deepEqual(ran, [
    "claude mcp get carrick",
    `claude mcp add --scope user --transport http carrick ${MCP_URL}`,
  ]);

  // A client that already holds the server is left exactly as it is: `mcp add`
  // refuses a name it already has, so the read is what decides.
  const second = machine({
    commands: ["claude"],
    directories: [path.join(HOME, ".claude")],
    statuses: { "claude mcp get carrick": 0 },
  });
  assert.equal(connectMcpClients(second.env)[0]?.state, "present");
  assert.deepEqual(second.ran, ["claude mcp get carrick"]);
});

test("a command that fails leaves the line to copy, and no claim", () => {
  const { env } = machine({
    commands: ["claude"],
    directories: [path.join(HOME, ".claude")],
    statuses: { "claude mcp get carrick": 1, [`claude mcp add --scope user --transport http carrick ${MCP_URL}`]: 2 },
  });
  const outcomes = connectMcpClients(env);
  assert.equal(outcomes[0]?.state, "failed");
  assert.equal(outcomes[0]?.detail, MCP_LINE);
  assert.ok(mcpLines(outcomes).some((line) => line.includes("could not")));
});

test("a client with no config file yet gets one in that client's own shape", () => {
  const { env, written } = machine({
    directories: [
      path.join(HOME, ".cursor"),
      path.join(HOME, ".codeium", "windsurf"),
      path.join(HOME, ".config", "Code", "User"),
    ],
  });
  const outcomes = connectMcpClients(env);
  assert.deepEqual(
    outcomes.map((outcome) => outcome.client),
    ["Cursor", "Windsurf", "VS Code"],
  );
  assert.deepEqual(JSON.parse(written[cursorFile]!), {
    mcpServers: { carrick: { url: MCP_URL } },
  });
  assert.deepEqual(JSON.parse(written[windsurfFile]!), {
    mcpServers: { carrick: { serverUrl: MCP_URL } },
  });
  assert.deepEqual(JSON.parse(written[vsCodeFile]!), {
    servers: { carrick: { type: "http", url: MCP_URL } },
  });
  for (const outcome of outcomes) assert.equal(outcome.state, "written");
});

test("a client that is not on this machine is not configured", () => {
  const { env, written } = machine({ directories: [path.join(HOME, ".cursor")] });
  assert.deepEqual(
    connectMcpClients(env).map((outcome) => outcome.client),
    ["Cursor"],
  );
  assert.deepEqual(Object.keys(written), [cursorFile]);
});

const cursor: FileClient = {
  name: "Cursor",
  directory: (env) => path.join(env.home, ".cursor"),
  file: (env) => path.join(env.home, ".cursor", "mcp.json"),
  container: "mcpServers",
  urlKey: "url",
  statesType: false,
};

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

test("a server already called carrick is left alone", () => {
  const existing = JSON.stringify({ mcpServers: { carrick: { url: "https://api.carrick.tools/mcp/p/one" } } }, null, 2);
  const result = mergeServerEntry(cursor, existing);
  assert.equal(result.state, "present");
  assert.deepEqual(JSON.parse(result.body), JSON.parse(existing));

  const { env, written } = machine({ directories: [path.join(HOME, ".cursor")], files: { [cursorFile]: existing } });
  assert.equal(connectMcpClients(env)[0]?.state, "present");
  assert.deepEqual(written, {});
});

test("a hand-edited file that no longer parses is reported, never replaced", () => {
  assert.throws(() => mergeServerEntry(cursor, "{ not json"));
  const { env, written } = machine({
    directories: [path.join(HOME, ".cursor")],
    files: { [cursorFile]: "{ not json" },
  });
  const outcome = connectMcpClients(env)[0];
  assert.equal(outcome?.state, "failed");
  assert.match(outcome!.detail, /not valid JSON/);
  assert.deepEqual(written, {});
});

test("every path written is printed", () => {
  const { env } = machine({ directories: [path.join(HOME, ".cursor")] });
  const lines = mcpLines(connectMcpClients(env)).join("\n");
  assert.ok(lines.includes(cursorFile));
  assert.ok(lines.includes(MCP_URL));
});
