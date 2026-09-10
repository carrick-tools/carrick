// The language server, driven over stdio against the fake CLI.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { LspClient } from "./lsp-client.ts";
import { fakeEnv, firstCall, fixturePath, makeWorkspace, sourceText } from "./helpers.ts";

type Row = { code?: string; severity: number; range: { start: { line: number; character: number } } };

/** What the client is left holding per file: `publishDiagnostics` replaces. */
function finalState(client: LspClient): Map<string, Row[]> {
  const state = new Map<string, Row[]>();
  for (const publish of client.publishes) state.set(publish.uri, publish.diagnostics as Row[]);
  return state;
}

function rowsFor(client: LspClient, suffix: string): Row[] {
  for (const [uri, rows] of finalState(client)) if (uri.endsWith(suffix)) return rows;
  return [];
}

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

// ------------------------------------------------- the noise budget, live
//
// carrick#879's rules as the client sees them, over stdio.

test("a client with its own boundary surface gets no per-file row, and the notification instead", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ env: fakeEnv() });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root, "Visual Studio Code", { boundarySurface: true });
  client.open(workspace.file);
  await client.waitFor(
    () => client.notifications.some((one) => one.method === "carrick/boundary"),
    "the boundary notification",
  );
  await client.settle(200);

  const edited = client.publishes.find((publish) => publish.uri.endsWith("routes/users.ts"));
  const codes = (edited?.diagnostics as Array<{ code?: string }>).map((row) => row.code);
  assert.equal(codes.includes("boundary"), false, "off the Problems list");
  assert.equal(codes.length, 2, "the two findings and nothing else");

  // Moved, not dropped: the same lines, on the workspace surface.
  const moved = client.notifications.find((one) => one.method === "carrick/boundary");
  const params = moved?.params as { service: string; lines: string[] };
  assert.equal(params.service, "user-service");
  assert.match(params.lines[0] ?? "", /A local index holds what the deterministic passes state/);
});

test("a client that states no boundary surface keeps the file-level fallback", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ env: fakeEnv() });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.waitFor(() => client.publishes.length >= 3, "diagnostics");
  const edited = client.publishes.find((publish) => publish.uri.endsWith("routes/users.ts"));
  const codes = (edited?.diagnostics as Array<{ code?: string }>).map((row) => row.code);
  assert.equal(codes.includes("boundary"), true);
});

test("turning a surface off takes its rows away on the next publish, not at the next restart", async (t) => {
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

  // No further edit, no restart: the setting alone re-publishes.
  client.configure({ diagnostics: false });
  await client.waitFor(
    () =>
      client.publishes
        .slice(before)
        .some((publish) => publish.uri.endsWith("routes/users.ts")),
    "a re-publish after the setting changed",
  );
  await client.settle(200);

  const last = client.publishes.filter((publish) => publish.uri.endsWith("routes/users.ts")).at(-1);
  const codes = (last?.diagnostics as Array<{ code?: string }>).map((row) => row.code);
  assert.deepEqual(codes, ["boundary"], "the findings are gone and the boundary is not");
  const consumer = client.publishes
    .filter((publish) => publish.uri.endsWith("order-service/src/clients/users.ts"))
    .at(-1);
  assert.deepEqual(consumer?.diagnostics, [], "and the mirrored rows are cleared too");
});

// ------------------------------------------------------------ code lenses
//
// A lens is a request, not a push, which is the whole reason it is answered in
// an install where the hook owns delivery (carrick#880).

test("the lens answers in an install where the hook owns delivery", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ args: ["--hooks-installed"], env: fakeEnv() });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.settle(300);
  assert.equal(client.publishes.length, 0, "the hook is still the one that pushes");

  const id = client.request("textDocument/codeLens", {
    textDocument: { uri: `file://${workspace.file}` },
  });
  await client.waitFor(() => client.responses.has(id), "a lens response");
  const lenses = client.responses.get(id) as Array<{ command?: { title: string } }>;
  assert.ok(lenses.length > 0, "the channel gate is about pushing, and this was asked for");
  for (const lens of lenses) assert.equal(/\b0\b/.test(lens.command?.title ?? ""), false);
});

test("carrick.codeLens off answers the request with an empty list", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ env: fakeEnv() });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root, "Visual Studio Code", { codeLens: false });
  const id = client.request("textDocument/codeLens", {
    textDocument: { uri: `file://${workspace.file}` },
  });
  await client.waitFor(() => client.responses.has(id), "a lens response");
  assert.deepEqual(client.responses.get(id), []);
});

test("a client that refreshes lenses is asked to when a setting changes", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ env: fakeEnv() });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root, "Visual Studio Code", undefined, {
    workspace: { codeLens: { refreshSupport: true } },
  });
  client.configure({ codeLens: false });
  await client.waitFor(
    () => client.notifications.some((one) => one.method === "workspace/codeLens/refresh"),
    "the refresh request",
  );
});

test("a lens request after a check runs no second CLI call (carrick#910)", async (t) => {
  const workspace = makeWorkspace();
  const argvLog = path.join(workspace.root, "argv.log");
  const client = new LspClient({ env: fakeEnv({ CARRICK_FAKE_ARGV_LOG: argvLog }) });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.waitFor(() => client.publishes.length >= 1, "the check the open ran");

  const id = client.request("textDocument/codeLens", {
    textDocument: { uri: `file://${workspace.file}` },
  });
  await client.waitFor(() => client.responses.has(id), "a lens response");
  assert.ok((client.responses.get(id) as unknown[]).length > 0, "the lens still answers");
  // Every surface reads one cached answer per file, and a lens arrives on every
  // open and every save, so a second CLI call here would be a per-save cost.
  assert.equal(fs.readFileSync(argvLog, "utf8").trim().split("\n").length, 1);
});

// --------------------------------------------------- the span, over the wire
//
// carrick#922. The unit tests state the rule; this states where the text comes
// from, which is the part only the server knows.

test("a row underlines the open document's line, not the one on disk (carrick#922)", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ env: fakeEnv() });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  // An unsaved buffer: the file on disk indents line 42 by two, and the text
  // the client holds indents it by six. What the user sees is the latter.
  const buffer = sourceText(130).split("\n");
  buffer[41] = "      const unsaved = edit();";
  await client.initialize(workspace.root);
  client.open(workspace.file, buffer.join("\n"));
  await client.waitFor(() => client.publishes.length >= 3, "diagnostics");

  const finding = rowsFor(client, "user-service/src/routes/users.ts")[0];
  assert.equal(finding?.code, "type_mismatch");
  assert.deepEqual(finding?.range, {
    start: { line: 41, character: 6 },
    end: { line: 41, character: 29 },
  });
});

test("a counterpart file nobody opened is underlined from the file on disk", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ env: fakeEnv() });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.waitFor(() => client.publishes.length >= 3, "diagnostics");

  const mirrored = rowsFor(client, "order-service/src/clients/users.ts")[0];
  const line18 = sourceText(130).split("\n")[17] ?? "";
  assert.deepEqual(mirrored?.range, {
    start: { line: 17, character: line18.length - line18.trimStart().length },
    end: { line: 17, character: line18.trimEnd().length },
  });
  assert.ok(line18.trimEnd().length > 1, "a stand-in file would prove nothing here");
});

// ------------------------------------------- two passes, one published state
//
// carrick#923. Opening the other side of a contract is a second check pass with
// an opinion about the first file, and it knows less about that file than the
// file's own pass does.

test("opening a counterpart does not take the first file's boundary away (carrick#923)", async (t) => {
  const workspace = makeWorkspace();
  const counterpart = path.join(workspace.root, "order-service/src/clients/users.ts");
  const fixtures = path.join(workspace.root, "fixtures.json");
  fs.writeFileSync(
    fixtures,
    JSON.stringify({
      "user-service/src/routes/users.ts": fixturePath("check-mismatch.json"),
      "order-service/src/clients/users.ts": fixturePath("check-counterpart.json"),
    }),
  );
  const client = new LspClient({ env: fakeEnv({ CARRICK_FAKE_FIXTURE_MAP: fixtures }) });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.waitFor(() => client.publishes.length >= 3, "the first file's diagnostics");
  const first = rowsFor(client, "user-service/src/routes/users.ts").map((row) => row.code);
  assert.deepEqual(first, ["type_mismatch", "method_mismatch", "boundary"]);

  client.open(counterpart);
  await client.waitFor(
    () => rowsFor(client, "order-service/src/clients/users.ts").some((row) => row.code === "boundary"),
    "the counterpart's own diagnostics",
  );
  await client.settle(300);

  // The whole ticket: the first file's rows are what its own check said, still,
  // and the boundary is the sentence that makes a short answer readable.
  assert.deepEqual(
    rowsFor(client, "user-service/src/routes/users.ts").map((row) => row.code),
    first,
  );
  // And its finding is stated once, not twice: the counterpart's pass mirrors
  // the same finding onto the same line, and that is the same row.
  const republished = client.publishes.filter((publish) =>
    publish.uri.endsWith("user-service/src/routes/users.ts"),
  );
  assert.equal(republished.length, 1, "a merge that says nothing new is not sent again");

  // The counterpart holds its own pass's rows, with the first pass's mirrored
  // row of the same finding folded into them rather than sitting beside it.
  const other = rowsFor(client, "order-service/src/clients/users.ts");
  assert.deepEqual(
    other.map((row) => row.code),
    ["type_mismatch", "boundary"],
  );
  assert.equal(other[0]?.range.start.line, 17);
});

test("a pass that stops finding something clears only its own rows", async (t) => {
  const workspace = makeWorkspace();
  const counterpart = path.join(workspace.root, "order-service/src/clients/users.ts");
  const fixtures = path.join(workspace.root, "fixtures.json");
  fs.writeFileSync(
    fixtures,
    JSON.stringify({
      "user-service/src/routes/users.ts": fixturePath("check-mismatch.json"),
      "order-service/src/clients/users.ts": fixturePath("check-counterpart.json"),
    }),
  );
  const client = new LspClient({ env: fakeEnv({ CARRICK_FAKE_FIXTURE_MAP: fixtures }) });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.waitFor(() => client.publishes.length >= 3, "the first file's diagnostics");
  client.open(counterpart);
  await client.waitFor(
    () => rowsFor(client, "order-service/src/clients/users.ts").some((row) => row.code === "boundary"),
    "the counterpart's own diagnostics",
  );

  // The counterpart's next check finds nothing at all. Its rows go; the rows
  // the OTHER pass put on it are still what that pass found.
  fs.writeFileSync(
    fixtures,
    JSON.stringify({
      "user-service/src/routes/users.ts": fixturePath("check-mismatch.json"),
      "order-service/src/clients/users.ts": fixturePath("check-silent.json"),
    }),
  );
  client.change(counterpart, 2);
  await client.waitFor(
    () => !rowsFor(client, "order-service/src/clients/users.ts").some((row) => row.code === "boundary"),
    "the counterpart's rows clearing",
  );
  await client.settle(300);

  assert.deepEqual(
    rowsFor(client, "order-service/src/clients/users.ts").map((row) => row.code),
    ["type_mismatch"],
    "the mirrored row from the other side stands, because that check still says so",
  );
  assert.deepEqual(
    rowsFor(client, "user-service/src/routes/users.ts").map((row) => row.code),
    ["type_mismatch", "method_mismatch", "boundary"],
  );
});
