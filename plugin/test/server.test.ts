// The language server, driven over stdio against the fake CLI.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { LspClient } from "./lsp-client.ts";
import { fakeEnv, firstCall, fixturePath, makeWorkspace } from "./helpers.ts";

test("didOpen publishes the check verdicts, on the file and on its counterparts", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ env: fakeEnv() });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.waitFor(() => client.publishes.length >= 3, "diagnostics");

  const uris = client.publishes.map((publish) => publish.uri);
  assert.ok(uris.some((uri) => uri.endsWith("user-service/src/routes/users.ts")));
  assert.ok(uris.some((uri) => uri.endsWith("order-service/src/clients/users.ts")));
  const edited = client.publishes.find((publish) => publish.uri.endsWith("routes/users.ts"));
  // two findings and the boundary
  assert.equal(edited?.diagnostics.length, 3);
});

test("the CLI runs from the workspace root with the file relative to it", async (t) => {
  const workspace = makeWorkspace();
  const argvLog = path.join(workspace.root, "argv.log");
  const client = new LspClient({ env: fakeEnv({ CARRICK_FAKE_ARGV_LOG: argvLog }) });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.waitFor(() => firstCall(argvLog) !== null, "a CLI call");

  const call = firstCall(argvLog);
  assert.ok(call);
  assert.deepEqual(call.argv, ["check", "user-service/src/routes/users.ts", "--json"]);
  assert.equal(fs.realpathSync(call.cwd), fs.realpathSync(workspace.root));
});

test("a client rooted inside one service is corrected to the workspace, and logged", async (t) => {
  const workspace = makeWorkspace();
  const argvLog = path.join(workspace.root, "argv.log");
  const client = new LspClient({
    env: fakeEnv({ CARRICK_FAKE_ARGV_LOG: argvLog, CARRICK_LOG_QUIET: "0" }),
  });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  // The cwd trap: rootUri follows the agent's shell, so it can name a service.
  await client.initialize(workspace.service);
  client.open(workspace.file);
  await client.waitFor(() => firstCall(argvLog) !== null, "a CLI call");

  const call = firstCall(argvLog);
  assert.ok(call);
  assert.equal(fs.realpathSync(call.cwd), fs.realpathSync(workspace.root));
  // stderr arrives on its own schedule, so wait for the line rather than
  // assuming it landed before the CLI call did.
  await client.waitFor(
    () => /has no \.carrick\/; using .* \(ancestor\)/.test(client.stderr),
    "the root correction in the log",
  );
});

test("a burst of didChange checks once", async (t) => {
  const workspace = makeWorkspace();
  const argvLog = path.join(workspace.root, "argv.log");
  const client = new LspClient({ env: fakeEnv({ CARRICK_FAKE_ARGV_LOG: argvLog }) });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  for (let version = 1; version <= 5; version += 1) client.change(workspace.file, version);
  await client.waitFor(() => firstCall(argvLog) !== null, "a CLI call");
  await client.settle(600);

  const calls = fs.readFileSync(argvLog, "utf8").trim().split("\n");
  assert.equal(calls.length, 1);
});

test("verdicts that clear stop being published, and the boundary stays", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ env: fakeEnv() });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.waitFor(() => client.publishes.length >= 3, "the first diagnostics");
  const before = client.publishes.length;

  // The next check finds nothing: every file the last one flagged is cleared.
  fs.writeFileSync(
    path.join(workspace.root, "clean.json"),
    fs.readFileSync(fixturePath("check-clean.json"), "utf8"),
  );
  const clean = new LspClient({
    env: fakeEnv({ CARRICK_FAKE_FIXTURE: path.join(workspace.root, "clean.json") }),
  });
  t.after(() => clean.stop());
  await clean.initialize(workspace.root);
  clean.open(workspace.file);
  await clean.waitFor(() => clean.publishes.length >= 1, "the boundary");
  await clean.settle(200);
  assert.equal(clean.publishes.length, 1, "only the checked file");
  assert.equal(clean.publishes[0]?.diagnostics.length, 1, "the boundary and nothing else");
  assert.ok(before >= 3);
});

test("the server publishes nothing when the hook owns delivery", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ args: ["--hooks-installed"], env: fakeEnv() });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.settle(400);
  assert.equal(client.publishes.length, 0);
});

test("CARRICK_CHANNEL=lsp makes the server publish even beside the hook", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({
    args: ["--hooks-installed"],
    env: fakeEnv({ CARRICK_CHANNEL: "lsp" }),
  });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.waitFor(() => client.publishes.length >= 1, "diagnostics");
});

test("a pull request for diagnostics is answered from the same check", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ env: fakeEnv() });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  const id = client.request("textDocument/diagnostic", {
    textDocument: { uri: `file://${workspace.file}` },
  });
  await client.waitFor(() => client.responses.has(id), "a pull response");
  const result = client.responses.get(id) as { kind: string; items: unknown[] };
  assert.equal(result.kind, "full");
  assert.equal(result.items.length, 3);
});

test("a CLI that fails publishes nothing and keeps the server alive", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ env: fakeEnv({ CARRICK_FAKE_EXIT: "3" }) });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.settle(300);
  assert.equal(client.publishes.length, 0);

  const id = client.request("textDocument/diagnostic", {
    textDocument: { uri: `file://${workspace.file}` },
  });
  await client.waitFor(() => client.responses.has(id), "the server still answering");
});
