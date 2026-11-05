// The prompt lookup (carrick#3375): one search-by-intent call on a task-sized
// prompt, named functions printed, nothing stored, nothing printed on failure.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test, { type TestContext } from "node:test";
import { runHook, fakeEnv } from "./helpers.ts";
import { carrickHooks, expectedCarrickHooks } from "../src/init/settings.ts";
import { promptLookup, lookupLine, namedRows } from "../src/hook/lookup.ts";
import { countLookup, readSession } from "../src/hook/reuse.ts";

const PROJECT = "proj_00000000-0000-0000-0000-000000000001";
const STUB = path.join(import.meta.dirname, "lookup-stub.mjs");
const TWELVE = "write a helper that parses a money amount string into integer cents please";
const MARKER = "zqxjvkwm-unique-marker";

function tmp(t: TestContext): string {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-lookup-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function workspace(t: TestContext, ids: string[] = [PROJECT]): string {
  const root = tmp(t);
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(
    path.join(root, ".carrick", "hosted", "snapshot.json"),
    JSON.stringify({
      resolution: { repos: ids.map((id, i) => ({ full_name: `o/r${i}`, connected: true, project_id: id })) },
    }),
  );
  return root;
}

type Sent = { url: string; body: Record<string, unknown>; headers: Record<string, string> };

function recorder(
  reply: () => Promise<Response> | Response,
): { request: typeof fetch; sent: Sent[] } {
  const sent: Sent[] = [];
  const request = (async (url: string, init: RequestInit) => {
    sent.push({
      url: String(url),
      body: JSON.parse(String(init.body)) as Record<string, unknown>,
      headers: init.headers as Record<string, string>,
    });
    return reply();
  }) as unknown as typeof fetch;
  return { request, sent };
}

const ROWS = () =>
  Response.json({
    results: [
      { name: "parseAmount", file_path: "core/src/money.ts", line_number: 12 },
      { name: "b", file_path: "b.ts", line_number: 2 },
      { name: "c", file_path: "c.ts", line_number: 3 },
      { name: "d", file_path: "d.ts", line_number: 4 },
    ],
  });

function options(t: TestContext, request: typeof fetch, extra: Partial<Parameters<typeof promptLookup>[0]> = {}) {
  return {
    prompt: TWELVE,
    session: "s1",
    root: workspace(t),
    home: tmp(t),
    env: { CARRICK_TOKEN: "tok" },
    request,
    log: () => {},
    ...extra,
  };
}

test("one call carries the first 1,000 characters, the project id and the invoker", async (t) => {
  const { request, sent } = recorder(ROWS);
  const long = `${TWELVE} ${"x".repeat(1500)}`;
  const line = await promptLookup(options(t, request, { prompt: long }));
  assert.equal(sent.length, 1);
  assert.equal(sent[0]!.body["action"], "search-by-intent");
  assert.equal(sent[0]!.body["project_id"], PROJECT);
  assert.equal(sent[0]!.body["query"], long.slice(0, 1000));
  assert.equal(sent[0]!.body["top_k"], 3);
  assert.equal(sent[0]!.headers["x-carrick-invoker"], "hook:user-prompt");
  assert.equal(
    line,
    "Carrick: functions that may already do part of this: `parseAmount` (`core/src/money.ts:12`), `b` (`b.ts:2`), `c` (`c.ts:3`). If one does the job, import it and call it rather than writing your own.",
  );
});

test("a slow answer is cut at the deadline and prints nothing", async (t) => {
  const { request } = recorder(() => new Promise((resolve) => setTimeout(() => resolve(ROWS()), 3500)));
  const started = Date.now();
  const line = await promptLookup(options(t, request));
  assert.equal(line, null);
  assert.ok(Date.now() - started < 3400, "cut near 3,000 ms");
});

test("401, 403 and 500 print nothing", async (t) => {
  for (const status of [401, 403, 500]) {
    const { request } = recorder(() => new Response("no", { status }));
    assert.equal(await promptLookup(options(t, request)), null, String(status));
  }
});

test("an 11-word prompt asks nothing", async (t) => {
  const { request, sent } = recorder(ROWS);
  const eleven = TWELVE.split(" ").slice(0, 11).join(" ");
  assert.equal(await promptLookup(options(t, request, { prompt: eleven })), null);
  assert.equal(sent.length, 0);
});

test("the 11th lookup in a session asks nothing", async (t) => {
  const { request, sent } = recorder(ROWS);
  const opts = options(t, request);
  for (let i = 0; i < 10; i += 1) assert.ok(await promptLookup(opts));
  assert.equal(sent.length, 10);
  assert.equal(await promptLookup(opts), null);
  assert.equal(sent.length, 10);
  assert.equal(readSession("s1", opts.home).lookups, 10);
});

test("no project id, or two distinct ones, asks nothing", async (t) => {
  for (const ids of [[], ["proj_a", "proj_b"]]) {
    const { request, sent } = recorder(ROWS);
    assert.equal(await promptLookup(options(t, request, { root: workspace(t, ids) })), null);
    assert.equal(sent.length, 0);
  }
});

test("the session record gains the count and the shown functions, never the prompt", async (t) => {
  const { request } = recorder(ROWS);
  const opts = options(t, request, { prompt: `${TWELVE} ${MARKER}` });
  await promptLookup(opts);
  const record = readSession("s1", opts.home);
  assert.equal(record.lookups, 1);
  assert.deepEqual(record.named_by_lookup, ["core/src/money.ts::parseAmount", "b.ts::b", "c.ts::c"]);
  assert.equal(countLookup("s1", opts.home), 2);
});

test("rows without a name or a file are skipped; none prints nothing", () => {
  assert.deepEqual(namedRows({ results: [{ name: "a" }, { file_path: "a.ts" }] }), []);
  assert.deepEqual(namedRows({}), []);
  assert.match(lookupLine([{ name: "a", where: "a.ts" }]), /`a` \(`a.ts`\)\./);
});

test("both hosts register the prompt hook", () => {
  const entry = carrickHooks()["UserPromptSubmit"]![0]!.hooks[0]!;
  assert.equal(entry.command, "carrick hook user-prompt");
  assert.equal(entry.timeout, 5);
  assert.ok(
    expectedCarrickHooks().some((h) => h.event === "UserPromptSubmit" && h.command === "carrick hook user-prompt"),
  );
});

function filesUnder(dir: string): string[] {
  return fs.readdirSync(dir, { withFileTypes: true, recursive: true })
    .filter((e) => e.isFile())
    .map((e) => path.join(e.parentPath, e.name));
}

test("the hook process stores and prints the prompt nowhere", async (t) => {
  const root = workspace(t);
  const home = tmp(t);
  const outside = tmp(t);
  const log = path.join(outside, "sent.jsonl");
  const prompt = `${TWELVE} ${MARKER}`;
  const run = await runHook("user-prompt.ts", {
    payload: { hook_event_name: "UserPromptSubmit", session_id: "priv-1", cwd: root, prompt },
    nodeArgs: ["--import", STUB],
    env: fakeEnv({
      HOME: home,
      CLAUDE_PROJECT_DIR: root,
      CARRICK_TOKEN: "tok",
      LOOKUP_STUB_LOG: log,
      CARRICK_LOG_QUIET: "0",
      CARRICK_LOG: path.join(home, "carrick.log"),
    }),
  });
  assert.equal(run.code, 0);
  const sent = JSON.parse(fs.readFileSync(log, "utf8").trim()) as { body: { query: string } };
  assert.equal(sent.body.query, prompt);
  const context = (JSON.parse(run.stdout) as { hookSpecificOutput: { additionalContext: string } })
    .hookSpecificOutput.additionalContext;
  assert.match(context, /`parseAmount` \(`core\/src\/money.ts:12`\)/);
  assert.doesNotMatch(context, /fourth/);
  assert.match(run.stderr, /lookup words=\d+ ms=\d+ rows=3 status=200/);
  // No 8-character window of the prompt is in any file under HOME, nor stderr.
  const windows: string[] = [];
  for (let i = 0; i + 8 <= prompt.length; i += 1) windows.push(prompt.slice(i, i + 8));
  for (const file of filesUnder(home)) {
    const text = fs.readFileSync(file, "utf8");
    for (const w of windows) assert.ok(!text.includes(w), `${file} holds a window of the prompt`);
  }
  for (const w of windows) assert.ok(!run.stderr.includes(w), "stderr holds a window of the prompt");
  assert.equal(readSession("priv-1", home).lookups, 1);
});

test("the hook process prints nothing on a refusal or past the deadline, and on Claude Code skips the Codex parts", async (t) => {
  for (const mode of ["401", "403", "500", "slow"]) {
    const root = workspace(t);
    const home = tmp(t);
    const run = await runHook("user-prompt.ts", {
      payload: { hook_event_name: "UserPromptSubmit", session_id: "f-1", cwd: root, prompt: TWELVE },
      nodeArgs: ["--import", STUB],
      env: fakeEnv({
        HOME: home,
        CLAUDE_PROJECT_DIR: root,
        CARRICK_TOKEN: "tok",
        LOOKUP_STUB_LOG: path.join(tmp(t), "s.jsonl"),
        LOOKUP_STUB_MODE: mode,
      }),
    });
    assert.equal(run.stdout, "", mode);
    assert.equal(run.code, 0);
    if (mode === "slow") assert.ok(run.ms < 4500, `held the prompt ${run.ms} ms`);
  }
});
