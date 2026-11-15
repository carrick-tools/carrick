// Which index each part of the hooks reads (carrick#3434, A2 T2): the fact
// lines read the fact file, the re-check, Read and uses lines read this
// machine's copy. When the two name different commits, the session start says
// so in its first line and the first part-2 line of the session says so once.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fakeEnv, fixturePath, runHook } from "./helpers.ts";
import { context, lineOf, patchEdit, workspace, type Ws } from "./facts-helpers.ts";
import { generationGap } from "../src/hosted-snapshot.ts";
import type { StatusResult } from "../src/contract.ts";

const LOCAL = "a".repeat(40);
const ORDERS = "export function loadOrder(id: string) {\n  const key = id;\n  return key;\n}\n";
const SENTENCE = /^Carrick's MCP tools and fact lines read the hosted index: api at [0-9a-f]{7} \(built [^)]*\)\. The re-check, Read, uses and search lines read this machine's copy: api at aaaaaaa \(scanned [^)]*\)\.$/;

/** A fixture dir with a status and a check answer whose index commit is `commit`, both for `/workspace/api`. */
function stampedFixtures(t: { after: (fn: () => void) => void }, commit: string): { status: string; check: string } {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-generation-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const status = JSON.parse(fs.readFileSync(fixturePath("status-workspace.json"), "utf8")) as StatusResult;
  status.services = status.services.slice(0, 1).map((service) => ({ ...service, repo: "/workspace/api", index_commit: commit, indexed_at: new Date(Date.now() - 8 * 86_400_000).toISOString() }));
  const check = JSON.parse(fs.readFileSync(fixturePath("check-uses.json"), "utf8")) as Record<string, unknown>;
  Object.assign(check, { repo: "/workspace/api", index_commit: commit, indexed_at: new Date(Date.now() - 8 * 86_400_000).toISOString() });
  fs.writeFileSync(path.join(dir, "status.json"), JSON.stringify(status));
  fs.writeFileSync(path.join(dir, "check.json"), JSON.stringify(check));
  return { status: path.join(dir, "status.json"), check: path.join(dir, "check.json") };
}

async function gapWorkspace(t: { after: (fn: () => void) => void }, localCommit: string | null): Promise<{ ws: Ws; fixtures: { status: string; check: string } }> {
  const ws = await workspace(t, { "api/src/orders.ts": ORDERS }, []);
  // The fact file names the repo's real commit; the local copy names `localCommit`, or the same one.
  const real = fs.readFileSync(path.join(ws.root, "api", ".git", "HEAD"), "utf8").startsWith("ref:")
    ? (await import("node:child_process")).execFileSync("git", ["-C", path.join(ws.root, "api"), "rev-parse", "HEAD"], { encoding: "utf8" }).trim()
    : "";
  return { ws, fixtures: stampedFixtures(t, localCommit ?? real) };
}

test("a fact file newer than the local index is said once at session start and once at the first edit", async (t) => {
  const { ws, fixtures } = await gapWorkspace(t, LOCAL);
  const env = { HOME: ws.home, CARRICK_FAKE_FIXTURE: fixtures.status };
  const start = await runHook("session-start.ts", { cwd: ws.root, env: fakeEnv(env) });
  assert.match(start.stdout.split("\n")[0] ?? "", SENTENCE, start.stdout);

  const file = ws.at("api/src/orders.ts");
  const at = lineOf(file, "const key");
  const edit = (probe: string) =>
    patchEdit(ws, file, [{ newStart: at, lines: ["+  const key = id;"] }], { session: "gen", env: { CARRICK_FAKE_FIXTURE: probe } });
  // A fact file answers the edit; its check is queued and arrives with the next carrier.
  await edit(fixtures.check);
  const carrier = async () => {
    const { waitForSessionAnswers } = await import("./helpers.ts");
    await waitForSessionAnswers(ws.home, "gen");
    const run = await runHook("post-bash.ts", {
      payload: { hook_event_name: "PostToolUse", tool_name: "Bash", session_id: "gen", cwd: ws.root, tool_input: { command: "ls" }, tool_response: { stdout: "" } },
      env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixtures.check }),
    });
    return context(run.stdout);
  };
  const first = await carrier();
  assert.match(first.split("\n\n")[0] ?? "", SENTENCE, first);
  assert.equal(first.match(/read the hosted index/g)?.length, 1, "said once in the first carrier");
  // A second edit's answer does not repeat it.
  await edit(fixtures.check);
  const second = await carrier();
  assert.doesNotMatch(second, /read the hosted index/, second);
});

test("no gap prints nothing", async (t) => {
  const { ws, fixtures } = await gapWorkspace(t, null);
  const start = await runHook("session-start.ts", { cwd: ws.root, env: fakeEnv({ HOME: ws.home, CARRICK_FAKE_FIXTURE: fixtures.status }) });
  assert.doesNotMatch(start.stdout, /read the hosted index/, start.stdout);
});

test("a repo with two fact commits is left out and logged", () => {
  const logged: string[] = [];
  const log = (...parts: unknown[]) => logged.push(parts.join(" "));
  const repos = [{ repo: "example/api", path: "/nowhere/api" }];
  // The fact file names no single commit for the repo (`commitsOf` drops it).
  assert.deepEqual(generationGap({}, repos, [{ repo: "/nowhere/api", commit: "b".repeat(40) }], log), []);
  assert.match(logged.join("\n"), /no single fact commit: example\/api/);
  // A local copy with no single commit is left out too.
  assert.deepEqual(generationGap({ "example/api": "c".repeat(40) }, repos, [{ repo: "/nowhere/api", commit: null }], log), []);
  assert.match(logged.join("\n"), /no single local commit: \/nowhere\/api/);
  // Two commits that differ are the gap.
  assert.deepEqual(generationGap({ "example/api": "c".repeat(40) }, repos, [{ repo: "/nowhere/api", commit: "b".repeat(40) }], log), [
    { repo: "example/api", fact: "c".repeat(40), local: "b".repeat(40), localIndexedAt: null },
  ]);
});

test("a written fact file starts the move", async (t) => {
  const { runFactsRefresh } = await import("../src/facts/fetch.ts");
  const ws = await workspace(t, {}, null);
  const status = JSON.parse(fs.readFileSync(fixturePath("status-workspace.json"), "utf8")) as StatusResult;
  status.services = status.services.map((service) => ({ ...service, hosted_state: "enriched" as const }));
  const env = { ...process.env, CARRICK_TOKEN: "t", CARRICK_FORWARD_COOLDOWN_MS: "0" };
  const started: string[][] = [];
  const forward = { status: async () => status, start: (_command: string, args: string[]) => void started.push(args) };
  const stage = (digest: string) => {
    const text = `${JSON.stringify({ schema: 1, writer: 1, project_id: "p", project_slug: "kinds-demo", digest, built_at: "2026-01-01T00:00:00Z", repos: [], counts: { rows: 0, producers: 0, consumers: 0 } })}\n`;
    return (async (url: string) =>
      String(url).endsWith("/mcp/facts")
        ? new Response(JSON.stringify({ success: true, staged_url: "https://x.s3.amazonaws.com/f" }))
        : new Response(text)) as unknown as typeof fetch;
  };
  assert.equal(await runFactsRefresh(ws.root, { request: stage("d1"), env, forward }), "written");
  assert.equal(started.length, 1);
  assert.deepEqual(started[0]!.slice(0, 2), ["refresh", "--hosted-only"]);
  // The digest the file already holds: the cloud says unchanged and nothing starts.
  const unchanged = (async () => new Response(JSON.stringify({ success: true, unchanged: true }))) as unknown as typeof fetch;
  assert.equal(await runFactsRefresh(ws.root, { request: unchanged, env, forward }), "unchanged");
  assert.equal(started.length, 1);
});
