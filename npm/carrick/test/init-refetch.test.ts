// A second `carrick init` in a folder that holds an index (carrick#5094): the
// index is read again when it was read for another project or cannot move to
// the hosted one, and the fact file the fetch leaves is the set-up project's.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { downloadHostedIndex, localIndexState } from "../src/init/hosted.ts";
import type { NativeRun } from "../src/init/hosted.ts";
import { parseStatusResult, STATUS_SCHEMA } from "../src/contract.ts";
import type { StatusResult } from "../src/contract.ts";
import { buildFacts, factsPath, loadSqlite } from "../src/facts/store.ts";
import { refreshFacts } from "../src/facts/fetch.ts";
import { factsAfterRefresh } from "../src/facts/after-refresh.ts";

/** A workspace whose cached hosted snapshot names `slug`, with an index on it. */
function workspace(t: { after: (fn: () => void) => void }, slug: string | null): string {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-refetch-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  if (slug !== null) {
    fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: slug }] } }));
  }
  return root;
}

function status(root: string, extra: Partial<StatusResult> = {}): StatusResult {
  return {
    schema: STATUS_SCHEMA,
    workspace: root,
    services: [{ service: "api", repo: root, index_commit: "abc1234", routes: 1, calls: 1, changed_since_index: 0, hosted_state: "enriched" }],
    repos: [{ repo: root, name: "api", changed_since_index: 0, outside_every_service: 0 }],
    ...extra,
  };
}

const kept = [{ repo: "api", path: "/code/api", line: "api's local index could not move.", reason_short: "history diverged from main" }];

test("an index read for another project is read again, and one read for this project is current", (t) => {
  const root = workspace(t, "project-a");
  const answer = status(root);
  assert.equal(localIndexState(answer, [root], { workspace: root, project: "project-b" }).kind, "reread");
  assert.deepEqual(localIndexState(answer, [root], { workspace: root, project: "project-a" }), { kind: "current" });
  // No project to compare, or none recorded yet: nothing says the index is for another one.
  assert.deepEqual(localIndexState(answer, [root], { workspace: root, project: null }), { kind: "current" });
  const bare = workspace(t, null);
  assert.deepEqual(localIndexState(status(bare), [bare], { workspace: bare, project: "project-b" }), { kind: "current" });
});

test("a local copy that cannot move to the hosted index is read again, by the status field and not its sentence", (t) => {
  const root = workspace(t, "project-a");
  const answer = status(root, { kept_back: kept });
  assert.equal(localIndexState(answer, [root], { workspace: root, project: "project-a" }).kind, "reread");
  // The same field survives the parse.
  const parsed = parseStatusResult(JSON.stringify({ schema: STATUS_SCHEMA, services: [], kept_back: kept }));
  assert.equal(parsed?.kept_back?.length, 1);
  assert.equal(parseStatusResult(JSON.stringify({ schema: STATUS_SCHEMA, services: [] }))?.kept_back, undefined);
});

function recordingRun(answer: StatusResult): { run: NativeRun; commands: string[] } {
  const commands: string[] = [];
  return {
    commands,
    run: (args) => {
      commands.push(args[0] ?? "");
      return Promise.resolve({ status: 0, stdout: args[0] === "status" ? JSON.stringify(answer) : "", stderr: "" });
    },
  };
}

test("init re-reads the hosted index when the project changed, adds no line of its own, and leaves a same-project folder alone", async (t) => {
  const root = workspace(t, "project-a");
  const moved = recordingRun(status(root));
  const said: string[] = [];
  const outcome = await downloadHostedIndex(root, moved.run, [root], (line) => void said.push(line), "project-b");
  assert.deepEqual(moved.commands, ["status", "refresh", "status"]);
  assert.deepEqual(outcome, { kind: "downloaded", services: 1, reread: true, readFromHosted: [] });
  assert.deepEqual(said, []);

  const same = recordingRun(status(root));
  const unchanged = await downloadHostedIndex(root, same.run, [root], null, "project-a");
  assert.deepEqual(same.commands, ["status"]);
  assert.equal(unchanged.kind === "downloaded" && unchanged.reread, false);

  const diverged = recordingRun(status(root, { kept_back: kept }));
  await downloadHostedIndex(root, diverged.run, [root], null, "project-a");
  assert.deepEqual(diverged.commands, ["status", "refresh", "status"]);
});

// The cloud's own answer for a project: a file for `slug` under digest `digest`.
function cloudFor(slug: string, digest: string, bodies: Array<Record<string, unknown>> = []): typeof fetch {
  const ndjson = `${JSON.stringify({ schema: 1, writer: 1, project_id: "p", project_slug: slug, digest, built_at: "2026-01-01T00:00:00Z", repos: [], counts: { rows: 0, producers: 0, consumers: 0 } })}\n`;
  return (async (url: string, init?: RequestInit) => {
    if (String(url).endsWith("/mcp/facts")) {
      const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
      bodies.push(body);
      if (body["project"] !== slug) return new Response(JSON.stringify({ success: false, error: { code: "no_facts" } }), { status: 404 });
      // Unchanged when the file on disk carries this digest, as the real answer does.
      if (body["known_digest"] === digest) return Response.json({ success: true, unchanged: true });
      return Response.json({ success: true, staged_url: "https://stage.s3.amazonaws.com/f" });
    }
    return new Response(ndjson);
  }) as typeof fetch;
}

async function factsProject(root: string): Promise<string | null> {
  const file = factsPath(root);
  if (!fs.existsSync(file)) return null;
  const sqlite = (await loadSqlite())!;
  const db = new sqlite.DatabaseSync(file, { readOnly: true });
  try {
    return String(db.prepare("SELECT value FROM meta WHERE key = 'project'").get()?.["value"]);
  } finally {
    db.close();
  }
}

/** A fact file for `slug` already on disk, as a first init left it. */
async function landed(root: string, slug: string, digest: string): Promise<void> {
  const header = JSON.stringify({ schema: 1, writer: 1, project_id: "p", project_slug: slug, digest, built_at: "2026-01-01T00:00:00Z", repos: [], counts: { rows: 0, producers: 0, consumers: 0 } });
  await buildFacts(`${header}\n`, factsPath(root), [], new Date().toISOString());
}

const env = { ...process.env, CARRICK_TOKEN: "t", CARRICK_NO_FACTS_REFRESH: "" };

test("a fact file for another project is replaced, whatever digest it carries", async (t) => {
  const root = workspace(t, "project-b");
  await landed(root, "project-a", "d1");
  // The cloud's file for B carries the same digest as A's: only the project tells them apart.
  assert.equal(await refreshFacts(root, { request: cloudFor("project-b", "d1"), env }), "written");
  assert.equal(await factsProject(root), "project-b");
});

test("a fact file for another project is not kept when the cloud has none for this one", async (t) => {
  const root = workspace(t, "project-b");
  await landed(root, "project-a", "d1");
  assert.equal(await refreshFacts(root, { request: cloudFor("project-c", "d9"), env }), "none");
  assert.equal(await factsProject(root), null);
});

test("a fact file for this project still answers unchanged and stays", async (t) => {
  const root = workspace(t, "project-b");
  await landed(root, "project-b", "d1");
  assert.equal(await refreshFacts(root, { request: cloudFor("project-b", "d1"), env }), "unchanged");
  assert.equal(await factsProject(root), "project-b");
});

test("after carrick refresh the fact file is the set-up project's, or none", async (t) => {
  const root = workspace(t, "project-b");
  await landed(root, "project-a", "d1");
  await factsAfterRefresh(["refresh", "--workspace", root], { request: cloudFor("project-b", "d2"), env });
  assert.equal(await factsProject(root), "project-b");
  const other = workspace(t, "project-b");
  await landed(other, "project-a", "d1");
  await factsAfterRefresh(["refresh", "--workspace", other], { request: cloudFor("project-c", "d2"), env });
  assert.equal(await factsProject(other), null);
});
