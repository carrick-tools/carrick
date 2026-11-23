// Daily usage counts (carrick#5083): what is built, what never leaves the
// machine, when nothing is sent, when a day is kept, and that a hook never waits.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fakeEnv, fixturePath, makeWorkspace, runHook } from "./helpers.ts";
import { readTimings, MAX_LINES } from "../src/hook/timings.ts";
import type { TimingRecord } from "../src/hook/timings.ts";
import { buildDayUsage, sendUsage, startUsageSend, usageGate, writeTelemetry } from "../src/hook/usage.ts";
import type { UsageRequest } from "../src/hook/usage.ts";
import { telemetry } from "../src/init/telemetry.ts";

const FIXTURE = path.join(import.meta.dirname, "fixtures", "usage", "hook-timings.jsonl");
const NOTICED = "2026-11-22T12:00:00.000Z";
const SINCE = Date.parse(NOTICED);
const NOW = Date.parse("2026-11-23T10:00:00.000Z");
const WORD = "secret-billing-name";

function tmp(prefix: string): string {
  return fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), `carrick-usage-${prefix}-`)));
}

/** A folder Carrick is set up in, holding the fixture's timings, and a home that has been told. */
function setup(options: { told?: boolean } = {}) {
  const root = tmp("root");
  fs.mkdirSync(path.join(root, ".carrick"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "index.json"), "{}");
  fs.copyFileSync(FIXTURE, path.join(root, ".carrick", "hook-timings.jsonl"));
  const home = tmp("home");
  if (options.told !== false) writeTelemetry({ state: "on", noticed_at: NOTICED }, home);
  const env: NodeJS.ProcessEnv = { CARRICK_TOKEN: "test-token", HOME: home };
  return { root, home, env };
}

function recorder(status: number | (() => Promise<number>)) {
  const calls: { url: string; body: string; headers: Record<string, string> }[] = [];
  const request: UsageRequest = async (url, init) => {
    calls.push({ url, body: init.body, headers: init.headers });
    return { status: typeof status === "number" ? status : await status() };
  };
  return { calls, request };
}

const day22 = {
  day: "2026-11-22",
  complete: true,
  hooks: {
    "session-start": { runs: 1, printed: 1, silent: 0, error: 0, p50_ms: 200, p90_ms: 200, waited_p50_ms: 0 },
    "post-read": { runs: 1, printed: 0, silent: 1, error: 0, p50_ms: 50, p90_ms: 50, waited_p50_ms: 0 },
    "post-edit": { runs: 3, printed: 1, silent: 1, error: 1, p50_ms: 600, p90_ms: 1800, waited_p50_ms: 100 },
    "post-bash": { runs: 1, printed: 0, silent: 1, error: 0, p50_ms: 70, p90_ms: 70, waited_p50_ms: 0 },
    recheck: { runs: 1, printed: 0, silent: 1, error: 0, p50_ms: 900, p90_ms: 900, waited_p50_ms: 800 },
    held: { runs: 1, printed: 0, silent: 1, error: 0, p50_ms: 30, p90_ms: 30, waited_p50_ms: 0 },
    "prompt-submit": { runs: 1, printed: 1, silent: 0, error: 0, p50_ms: 20, p90_ms: 20, waited_p50_ms: 0 },
    stop: { runs: 1, printed: 1, silent: 0, error: 0, p50_ms: 25, p90_ms: 25, waited_p50_ms: 0 },
    "subagent-stop": { runs: 1, printed: 0, silent: 1, error: 0, p50_ms: 15, p90_ms: 15, waited_p50_ms: 0 },
  },
};
const day23 = {
  day: "2026-11-23",
  complete: true,
  hooks: {
    "session-start": { runs: 1, printed: 1, silent: 0, error: 0, p50_ms: 300, p90_ms: 300, waited_p50_ms: 0 },
    "post-edit": { runs: 2, printed: 1, silent: 1, error: 0, p50_ms: 410, p90_ms: 1830, waited_p50_ms: 0 },
  },
};

test("1. the fixture's days are counted from the notice on, in the closed shape", () => {
  const rows = readTimings(setup().root);
  assert.deepEqual(buildDayUsage(rows, "2026-11-22", SINCE), day22);
  assert.deepEqual(buildDayUsage(rows, "2026-11-23", SINCE), day23);
  assert.equal(buildDayUsage(rows, "2026-11-21", SINCE), null);
  // Nothing before the notice is counted: with the notice after the whole day, the day is empty.
  assert.equal(buildDayUsage(rows, "2026-11-22", Date.parse("2026-11-22T23:00:00Z")), null);
});

test("1b. a day that begins after midnight in a full file is not complete", () => {
  const rows: TimingRecord[] = Array.from({ length: MAX_LINES }, (_, i) => ({
    hook: "post-edit",
    start: new Date(Date.parse("2026-11-23T09:00:00Z") + i * 1000).toISOString(),
    duration_ms: 10,
    waited_ms: 0,
    outcome: "silent",
  }));
  assert.equal(buildDayUsage(rows, "2026-11-23", 0)?.complete, false);
  assert.equal(buildDayUsage(rows.slice(1), "2026-11-23", 0)?.complete, true);
});

const SCHEMA = {
  top: ["v", "install_id", "folder_id", "cli_version", "days"],
  day: ["day", "complete", "hooks"],
  hooks: ["session-start", "post-read", "post-edit", "post-bash", "recheck", "held", "prompt-submit", "stop", "subagent-stop"],
  hook: ["runs", "printed", "silent", "error", "p50_ms", "p90_ms", "waited_p50_ms"],
};

test("2. never-send: the body holds only the allowlist", async () => {
  const { root, home, env } = setup();
  const { calls, request } = recorder(200);
  assert.equal(await sendUsage(root, { env, home, now: NOW, request, version: "0.3.179" }), "sent");
  assert.equal(calls.length, 1);
  assert.equal(calls[0]!.url, "https://api.carrick.tools/mcp/usage");
  assert.equal(calls[0]!.headers["Authorization"], "Bearer test-token");
  const text = calls[0]!.body;
  const forbidden = [WORD, "src/", ".ts", "facts_stops", "held_state", "held_ms", "held_rows", "engine_ready", "facts_ms", "file", "mystery", root, home];
  for (const word of [...forbidden, os.hostname(), os.userInfo().username].filter((w) => w.length >= 4 || w === ".ts" || w === "src/")) {
    assert.ok(!text.includes(word), `the body must not contain ${word}`);
  }
  const body = JSON.parse(text);
  const keys = (value: object, allowed: string[]) => assert.ok(Object.keys(value).every((k) => allowed.includes(k)), `unexpected key in ${Object.keys(value)}`);
  keys(body, SCHEMA.top);
  assert.equal(Object.keys(body).length, SCHEMA.top.length);
  for (const entry of body.days) {
    assert.deepEqual(Object.keys(entry), SCHEMA.day);
    keys(entry.hooks, SCHEMA.hooks);
    for (const hook of Object.values(entry.hooks) as object[]) assert.deepEqual(Object.keys(hook), SCHEMA.hook);
  }
  assert.deepEqual(body.days, [day22, day23]);
  assert.match(body.install_id, /^[A-Za-z0-9_-]{8,64}$/);
  assert.match(body.folder_id, /^[A-Za-z0-9_-]{8,64}$/);
  assert.notEqual(body.folder_id, body.install_id);
  // What left the machine is what a person can read afterwards.
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(root, ".carrick", "usage-last.json"), "utf8")), body);
});

test("3. gate: every off switch sends nothing and creates no id", async () => {
  const cases: [string, NodeJS.ProcessEnv, "told" | "off" | "never"][] = [
    ["CARRICK_TELEMETRY=0", { CARRICK_TELEMETRY: "0" }, "told"],
    ["DO_NOT_TRACK=1", { DO_NOT_TRACK: "1" }, "told"],
    ["CI", { CI: "true" }, "told"],
    ["state off", {}, "off"],
    ["never told", {}, "never"],
  ];
  for (const [name, extra, kind] of cases) {
    const { root, home, env } = setup({ told: kind === "told" });
    if (kind === "off") writeTelemetry({ state: "off", noticed_at: NOTICED }, home);
    const { calls, request } = recorder(200);
    assert.equal(await sendUsage(root, { env: { ...env, ...extra }, home, now: NOW, request, version: "0.3.179" }), "none", name);
    assert.equal(calls.length, 0, name);
    assert.ok(!fs.existsSync(path.join(home, ".carrick", "install-id")), `${name}: no install id`);
    assert.ok(!fs.existsSync(path.join(root, ".carrick", "usage-folder-id")), `${name}: no folder id`);
    assert.equal(startUsageSend(root, { env: { ...env, ...extra }, home, now: NOW, start: () => assert.fail(name) }), false, name);
  }
  const { root, home, env } = setup();
  const { calls, request } = recorder(200);
  assert.equal(await sendUsage(root, { env, home, now: NOW, request, version: "0.3.179" }), "sent");
  assert.equal(calls.length, 1);
  assert.equal(usageGate({ ...env }, home).send, true);
});

test("4. the day is marked on 200 and 400 only, and kept on every other answer", async () => {
  for (const [status, marked] of [[200, true], [400, true], [401, false], [404, false], [429, false], [503, false]] as const) {
    const { root, home, env } = setup();
    const { calls, request } = recorder(status);
    await sendUsage(root, { env, home, now: NOW, request, version: "0.3.179" });
    assert.equal(fs.existsSync(path.join(root, ".carrick", "last-usage-final-day")), marked, `HTTP ${status}`);
    await sendUsage(root, { env, home, now: NOW, request, version: "0.3.179" });
    const second = JSON.parse(calls[1]!.body);
    assert.equal(second.days.length, marked ? 1 : 2, `HTTP ${status}: the kept day rides the next send`);
  }
  const { root, home, env } = setup();
  const thrown: UsageRequest = async () => {
    throw new Error("network down");
  };
  assert.equal(await sendUsage(root, { env, home, now: NOW, request: thrown, version: "0.3.179" }), "kept");
  assert.ok(!fs.existsSync(path.join(root, ".carrick", "last-usage-final-day")));
});

test("4b. a second start within the hour starts nothing", () => {
  const { root, home, env } = setup();
  const started: string[][] = [];
  const start = (_command: string, args: string[]) => void started.push(args);
  assert.equal(startUsageSend(root, { env, home, now: NOW, start }), true);
  assert.equal(startUsageSend(root, { env, home, now: NOW + 59 * 60_000, start }), false);
  assert.equal(startUsageSend(root, { env, home, now: NOW + 61 * 60_000, start }), true);
  assert.equal(started.length, 2);
  assert.ok(started[0]![0]!.includes("usage-run"));
  assert.equal(started[0]![1], root);
});

test("5. the first session start shows the notice once, then prints plain text", async () => {
  const workspace = makeWorkspace();
  const home = tmp("home");
  const run = () =>
    runHook("session-start.ts", {
      cwd: workspace.root,
      payload: { hook_event_name: "SessionStart" },
      env: fakeEnv({
        CLAUDE_PROJECT_DIR: workspace.root,
        CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json"),
        HOME: home,
        CARRICK_TELEMETRY: "1",
        DO_NOT_TRACK: "",
        CI: "",
        CARRICK_TOKEN: "",
        CARRICK_NO_FACTS_REFRESH: "1",
      }),
    });
  const first = await run();
  const parsed = JSON.parse(first.stdout);
  assert.match(parsed.systemMessage, /carrick telemetry off/);
  assert.match(parsed.systemMessage, /no code, file names or paths/);
  assert.equal(parsed.hookSpecificOutput.hookEventName, "SessionStart");
  assert.ok(JSON.parse(fs.readFileSync(path.join(home, ".carrick", "telemetry"), "utf8")).noticed_at);
  const second = await run();
  assert.ok(!second.stdout.trimStart().startsWith("{"), second.stdout);
  assert.ok(!second.stdout.includes("telemetry"), second.stdout);
});

test("6. the send never holds a hook: a silent server only costs the child its timeout", async () => {
  const { root, home, env } = setup();
  const begun = Date.now();
  assert.equal(startUsageSend(root, { env, home, now: NOW, start: () => undefined }), true);
  assert.ok(Date.now() - begun < 500);
  const silent: UsageRequest = (_url, init) =>
    new Promise((_resolve, reject) => init.signal.addEventListener("abort", () => reject(new Error("aborted"))));
  assert.equal(await sendUsage(root, { env, home, now: NOW, request: silent, version: "0.3.179", timeoutMs: 50 }), "kept");
  assert.ok(!fs.existsSync(path.join(root, ".carrick", "last-usage-final-day")));
});

test("7. carrick telemetry on, off and status", () => {
  const home = tmp("home");
  const root = setup().root;
  let out = "";
  const run = (args: string[], env: NodeJS.ProcessEnv = {}) => {
    out = "";
    const code = telemetry(args, { env, home, cwd: root, write: (text) => void (out += text) });
    return code;
  };
  assert.equal(run(["status"]), 0);
  assert.match(out, /nothing is counted until you have been told/);
  assert.equal(run(["off"]), 0);
  assert.equal(usageGate({}, home).reason, "off");
  assert.equal(run(["on"]), 0);
  assert.equal(usageGate({}, home).reason, "ok");
  assert.equal(run(["status"], { DO_NOT_TRACK: "1" }), 0);
  assert.match(out, /DO_NOT_TRACK is set, so nothing is sent/);
  assert.match(out, /Nothing has been sent from this folder yet/);
  assert.equal(run(["bogus"]), 2);
});
