// Cross-repo go to definition (carrick#881).
//
// Every test here is one of the noise rules the ticket accepts the feature on,
// named in the test title, so a rule that stops holding fails by name.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { coversPosition, definitionsAt } from "../src/definition.ts";
import { DEFAULT_SURFACES, readSurfaces } from "../src/surfaces.ts";
import { LspClient } from "./lsp-client.ts";
import { fakeEnv, fixture, fixturePath, makeWorkspace } from "./helpers.ts";

const mismatch = fixture("check-mismatch.json");
const onDisk = () => true;

/** Where the payload's rows are, 0-based, as an editor counts. */
const ROUTE = { line: 41, character: 2 }; // GET /api/users/:id, line 42 col 3
const CANDIDATE_CALL = { line: 60, character: 8 }; // POST /api/orders, line 61 col 9
const UNRESOLVABLE_CALL = { line: 79, character: 4 }; // GET /api/audit/:id, line 80 col 5

function localPath(location: { uri: string }): string {
  return fileURLToPath(location.uri);
}

// ------------------------------------------------------------ fall through

test("fall through: a position on no row's line answers nothing", () => {
  // Three lines above the route: the import a person actually asked about.
  assert.deepEqual(definitionsAt(mismatch, { line: 38, character: 9 }, { exists: onDisk }), []);
});

test("fall through: a position left of the row's anchor is not on the row", () => {
  assert.deepEqual(
    definitionsAt(mismatch, { line: ROUTE.line, character: 0 }, { exists: onDisk }),
    [],
  );
  assert.equal(coversPosition(mismatch.items?.[0] as never, { line: 41, character: 1 }), false);
  assert.equal(coversPosition(mismatch.items?.[0] as never, ROUTE), true);
  // The anchor column starts the span and everything right of it is inside.
  assert.equal(coversPosition(mismatch.items?.[0] as never, { line: 41, character: 60 }), true);
});

test("fall through: a row with no line claims no position", () => {
  const item = { kind: "call", counterparts: [{ role: "producer", repo: "/r", file: "a.ts" }] };
  assert.equal(coversPosition(item as never, { line: 0, character: 0 }), false);
});

// --------------------------------------------------------- never a guess

test("never a guessed path: a counterpart that is not on this disk yields no location", () => {
  assert.deepEqual(definitionsAt(mismatch, ROUTE, { exists: () => false }), []);
});

test("never a guessed path: a counterpart whose repo the index lost is left out", () => {
  // The route's two consumers: one with a repo, one with `repo: null`.
  const locations = definitionsAt(mismatch, ROUTE, { exists: onDisk });
  assert.equal(locations.length, 1);
  assert.equal(localPath(locations[0] as never), path.resolve("/workspace/order-service/src/clients/users.ts"));
});

// ------------------------------------------------------------ both directions

test("both directions: a route answers with its consumers", () => {
  const locations = definitionsAt(mismatch, ROUTE, { exists: onDisk });
  assert.equal(locations.length, 1);
  // The consumer's own line, 0-based, and column 0: the payload states no column.
  assert.deepEqual(locations[0]?.range.start, { line: 17, character: 0 });
});

test("both directions: a call answers with its producer", () => {
  const locations = definitionsAt(mismatch, CANDIDATE_CALL, { exists: onDisk });
  assert.equal(locations.length, 1);
  assert.equal(localPath(locations[0] as never), path.resolve("/workspace/order-service/src/server.ts"));
  assert.deepEqual(locations[0]?.range.start, { line: 119, character: 0 });
});

// ------------------------------------------------------------ candidates

test("a candidate answers a jump where nothing else does", () => {
  // The POST /api/orders row is the model's reading and it is the only row on
  // that line, so the jump is offered.
  assert.equal(definitionsAt(mismatch, CANDIDATE_CALL, { exists: onDisk }).length, 1);
});

test("a candidate is never the only answer where a fact row also matches", () => {
  const both = fixture("check-definition.json");
  // The candidate is first in `items`; the fact's location still leads.
  const locations = definitionsAt(both, { line: 29, character: 5 }, { exists: onDisk });
  assert.equal(locations.length, 2);
  assert.equal(localPath(locations[0] as never), path.resolve("/workspace/order-service/src/clients/users.ts"));
  assert.equal(localPath(locations[1] as never), path.resolve("/workspace/order-service/src/server.ts"));
});

// ------------------------------------------------------------ the setting

test("carrick.definition is on unless the client says otherwise", () => {
  // The switch lives with the others (carrick#879), and every shape a client
  // states them in reads the same.
  assert.equal(DEFAULT_SURFACES.definition, true);
  assert.equal(readSurfaces(undefined).definition, true);
  assert.equal(readSurfaces({}).definition, true);
  assert.equal(readSurfaces({ definition: false }).definition, false);
  assert.equal(readSurfaces({ settings: { carrick: { definition: false } } }).definition, false);
  // Not a boolean is not an answer, so the default stands.
  assert.equal(readSurfaces({ definition: "off" }).definition, true);
  // And turning this one off leaves the rest exactly as they were.
  assert.equal(readSurfaces({ definition: false }).diagnostics, true);
});

// ------------------------------------------------------------ over the wire

async function definitionAt(
  client: LspClient,
  file: string,
  position: { line: number; character: number },
): Promise<Array<{ uri: string; range: { start: { line: number } } }>> {
  const id = client.definition(file, position.line, position.character);
  await client.waitFor(() => client.responses.has(id), "a definition response");
  return client.responses.get(id) as never;
}

test("a call site jumps into the other repo on this disk", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ env: fakeEnv() });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.waitFor(() => client.publishes.length >= 1, "the first check");

  const locations = await definitionAt(client, workspace.file, CANDIDATE_CALL);
  assert.equal(locations.length, 1);
  // The CLI states the counterpart's repo absolutely, and on macOS the
  // temporary workspace has a `/var` symlink in front of it, so both sides are
  // compared as the paths the filesystem resolves to.
  assert.equal(
    fs.realpathSync(fileURLToPath(locations[0]?.uri as string)),
    fs.realpathSync(path.join(workspace.root, "order-service", "src", "server.ts")),
  );
});

test("a definition request on an import three lines above the row is empty", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ env: fakeEnv() });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.waitFor(() => client.publishes.length >= 1, "the first check");

  assert.deepEqual(await definitionAt(client, workspace.file, { line: 38, character: 9 }), []);
});

test("a counterpart repo that is absent from this machine yields no location", async (t) => {
  // makeWorkspace lays down order-service and billing-service and no
  // audit-service, which is the repo the /api/audit/:id row names.
  const workspace = makeWorkspace();
  const client = new LspClient({ env: fakeEnv() });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.waitFor(() => client.publishes.length >= 1, "the first check");

  assert.ok(!fs.existsSync(path.join(workspace.root, "audit-service")));
  assert.deepEqual(await definitionAt(client, workspace.file, UNRESOLVABLE_CALL), []);
});

test("the jump is answered where the hook owns delivery", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ args: ["--hooks-installed"], env: fakeEnv() });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.settle(300);
  assert.equal(client.publishes.length, 0, "the hook channel publishes nothing");

  const locations = await definitionAt(client, workspace.file, ROUTE);
  assert.equal(locations.length, 1);
});

test("the answer is warm: one CLI call for the check and the jump, under 300 ms", async (t) => {
  const workspace = makeWorkspace();
  const argvLog = path.join(workspace.root, "argv.log");
  // A CLI slow enough that a second call could not hide inside the budget.
  const client = new LspClient({
    env: fakeEnv({ CARRICK_FAKE_ARGV_LOG: argvLog, CARRICK_FAKE_DELAY_MS: "1200" }),
  });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.waitFor(() => client.publishes.length >= 1, "the first check");

  const started = Date.now();
  const locations = await definitionAt(client, workspace.file, ROUTE);
  const elapsed = Date.now() - started;
  assert.equal(locations.length, 1);
  assert.ok(elapsed < 300, `the jump took ${elapsed}ms`);
  const calls = fs.readFileSync(argvLog, "utf8").trim().split("\n");
  assert.equal(calls.length, 1, "the jump ran the CLI again");
});

test("an edit drops the cached answer, so a jump is never off a stale line", async (t) => {
  const workspace = makeWorkspace();
  const argvLog = path.join(workspace.root, "argv.log");
  // The hook channel, where nothing re-checks the file on a change: the drop
  // has to happen on the notification or the next jump reads the old lines.
  const client = new LspClient({
    args: ["--hooks-installed"],
    env: fakeEnv({ CARRICK_FAKE_ARGV_LOG: argvLog }),
  });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  await definitionAt(client, workspace.file, ROUTE);
  client.change(workspace.file, 2);
  await definitionAt(client, workspace.file, ROUTE);

  const calls = fs.readFileSync(argvLog, "utf8").trim().split("\n");
  assert.equal(calls.length, 2);
});

test("carrick.definition off answers nothing, and the capability stays", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ env: fakeEnv() });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root, "Claude Code", { definition: false });
  assert.equal(client.initializeResult?.capabilities?.["definitionProvider"], true);
  client.open(workspace.file);
  await client.waitFor(() => client.publishes.length >= 1, "the first check");

  assert.deepEqual(await definitionAt(client, workspace.file, ROUTE), []);
});

test("a client that sends its settings later turns the jump off without a restart", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ env: fakeEnv() });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  client.open(workspace.file);
  await client.waitFor(() => client.publishes.length >= 1, "the first check");
  assert.equal((await definitionAt(client, workspace.file, ROUTE)).length, 1);

  client.notify("workspace/didChangeConfiguration", {
    settings: { carrick: { definition: false } },
  });
  assert.deepEqual(await definitionAt(client, workspace.file, ROUTE), []);
});

test("CARRICK_CHANNEL=off silences the jump too", async (t) => {
  const workspace = makeWorkspace();
  const client = new LspClient({ env: fakeEnv({ CARRICK_CHANNEL: "off" }) });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  assert.deepEqual(await definitionAt(client, workspace.file, ROUTE), []);
});

test("a file the index does not hold answers nothing, and is asked again once it does", async (t) => {
  const workspace = makeWorkspace();
  // The CLI reads its fixture on every call, so rewriting the file is a user
  // running `carrick index` in a terminal between two jumps.
  const answer = path.join(workspace.root, "answer.json");
  fs.copyFileSync(fixturePath("check-not-indexed.json"), answer);
  const client = new LspClient({ env: fakeEnv({ CARRICK_FAKE_FIXTURE: answer }) });
  t.after(() => {
    client.stop();
    workspace.cleanup();
  });

  await client.initialize(workspace.root);
  assert.deepEqual(await definitionAt(client, workspace.file, ROUTE), []);

  fs.copyFileSync(fixturePath("check-mismatch.json"), answer);
  assert.equal((await definitionAt(client, workspace.file, ROUTE)).length, 1);
});

test("a hosted-only counterpart never becomes a local absolute-file jump", () => {
  const result = structuredClone(mismatch);
  const item = result.items?.[0];
  assert.ok(item);
  item.counterparts = [{
    role: "producer", service: "hosted-api", repo: null,
    file: "/workspace/local-existing.ts", line: 4,
    ...{ remote: "example/hosted-api" },
  }];
  assert.deepEqual(definitionsAt(result, ROUTE, { exists: onDisk }), []);
});
