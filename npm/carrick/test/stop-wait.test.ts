// The Stop hook delivers the background re-checks of a session's edits and
// never waits for one (`src/hook/stop-wait.ts`, B3; hook-wording audit T2).
// The runner is stood in for by the test: it writes the answer file the real
// runner would, at the moment the test names, and a live lock keeps the hook
// from starting a runner of its own.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { spawn } from "node:child_process";

import { asStated, fakeEnv, fixture, fixturePath, makeWorkspace, pluginDir, runHook, type HookRun } from "./helpers.ts";
import { parseChangedResult, type CheckResult } from "../src/contract.ts";
import {
  answerEarly,
  answerInHand,
  enqueue,
  enqueueChanged,
  lockPath,
  queueDir,
  STOP_TIMEOUT_S,
} from "../src/hook/rechecks.ts";
import type { Answer, QueueEntry } from "../src/hook/rechecks.ts";
import { checkedSinceLastEdit, checkMarkerPath } from "../src/hook/checked.ts";
import { readSession, recordEdit, sessionRecords, sessionsDir, writeAtomic } from "../src/hook/reuse.ts";
import { stopAnswers } from "../src/hook/stop-wait.ts";
import { adoptedLine, deliverLate, hasFindings } from "../src/hook/late.ts";
import type { HeldLine } from "../src/held/daemon.ts";
import { changedLines } from "../src/render.ts";
import { carrickHooks } from "../src/init/settings.ts";

const SESSION = "stop-sess";

function setup(t: { after: (fn: () => void) => void }) {
  const workspace = makeWorkspace();
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-home-")));
  t.after(() => {
    workspace.cleanup();
    fs.rmSync(home, { recursive: true, force: true });
  });
  return { workspace, home };
}

/** A live process holding the runner lock: the hook starts no runner of its own. */
function liveRunner(root: string): void {
  fs.mkdirSync(path.dirname(lockPath(root)), { recursive: true });
  const now = new Date().toISOString();
  fs.writeFileSync(lockPath(root), `${JSON.stringify({ pid: process.pid, started: now, touched: now })}\n`);
}

function queue(root: string, home: string, file: string, session = SESSION): QueueEntry {
  return enqueue(root, session, [file], Date.now(), { home })[0]!;
}

/** What the runner writes when a call ends. */
function answer(home: string, entry: QueueEntry, result: CheckResult): void {
  const body: Answer = {
    seq: entry.seq,
    session: entry.session,
    file: entry.file,
    relative: entry.relative,
    root: entry.root,
    state: "answered",
    enqueued_at: entry.enqueued_at,
    started_at: entry.enqueued_at,
    finished_at: new Date().toISOString(),
    ...(entry.instead_of_probe ? { instead_of_probe: true as const } : {}),
    result,
  };
  writeAtomic(path.join(sessionsDir(home), `${entry.session}.recheck.${entry.seq}.answer.json`), `${JSON.stringify(body)}\n`);
}

/** A mismatch the re-check answered for, its judge stating both as the edit's: its problems print. */
function problem(): CheckResult {
  return asStated({ ...fixture("check-mismatch.json"), recheck: { ran: "extraction+types", elapsed_ms: 1 } });
}

function clean(): CheckResult {
  return fixture("check-clean.json");
}

/**
 * Run the Stop hook the way a settings entry does. An older install's entry
 * still carries `--wait-s 200`; Stop ignores it.
 */
function stopWith(home: string, payload: Record<string, unknown> = {}, args: string[] = ["--wait-s", "200"]): Promise<HookRun> {
  const started = Date.now();
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [path.join(pluginDir, "src", "hook", "stop.ts"), ...args], {
      env: fakeEnv({ HOME: home }),
      stdio: ["pipe", "pipe", "pipe"],
    });
    let stdout = "";
    child.stdout.on("data", (chunk: Buffer) => (stdout += chunk.toString("utf8")));
    child.on("close", (code) => resolve({ stdout, stderr: "", code, ms: Date.now() - started }));
    child.stdin.end(JSON.stringify({ hook_event_name: "Stop", session_id: SESSION, ...payload }));
  });
}

function context(stdout: string): string {
  return (JSON.parse(stdout) as { hookSpecificOutput: { additionalContext: string } }).hookSpecificOutput
    .additionalContext;
}

/**
 * A spawned hook's wall time includes node's own start, which a loaded
 * machine stretches past a second; Stop's own work is timed in-process below.
 */
const SPAWNED_MS = 2500;

test("Stop's own work with a pending re-check takes under 1 s", (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  queue(workspace.root, home, workspace.file);
  const started = performance.now();
  const result = stopAnswers(SESSION, { home });
  const ms = performance.now() - started;
  assert.ok(ms < 1000, `Stop never waits (${Math.round(ms)} ms)`);
  assert.deepEqual(result.lines, [], "a re-check still running is not Stop's to say");
});

/** The hook events after a Stop that carry late answers, as Claude Code runs them. */
function promptHook(home: string, root: string): Promise<HookRun> {
  return runHook("user-prompt.ts", {
    payload: { hook_event_name: "UserPromptSubmit", session_id: SESSION, cwd: root, prompt: "go on" },
    env: fakeEnv({ HOME: home }),
  });
}

function bashHook(home: string, root: string, command = "git status"): Promise<HookRun> {
  return runHook("post-bash.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Bash",
      session_id: SESSION,
      cwd: root,
      tool_input: { command },
      tool_response: { stdout: "" },
    },
    env: fakeEnv({ HOME: home }),
    cwd: root,
  });
}

test("Stop with a pending re-check returns at once, ignores an older --wait-s, and names the file it did not check", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  queue(workspace.root, home, workspace.file);

  const run = await stopWith(home);
  assert.equal(run.code, 0);
  assert.ok(run.ms < SPAWNED_MS, `Stop never waits (${run.ms} ms)`);
  assert.equal(run.stdout, "", "a re-check still running prints nothing at Stop");
  assert.equal(changedQueued(workspace.root).length, 0, "the running re-check's answer rides the next hook event: no check --changed");
});

test("a finding that lands after Stop is printed once, by the next prompt", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  const entry = queue(workspace.root, home, workspace.file);
  assert.equal((await stopWith(home)).stdout, "", "running at Stop: silent");

  answer(home, entry, problem());
  const prompt = await promptHook(home, workspace.root);
  assert.equal(prompt.code, 0);
  const said = (JSON.parse(prompt.stdout) as { hookSpecificOutput: { hookEventName: string; additionalContext: string } })
    .hookSpecificOutput;
  assert.equal(said.hookEventName, "UserPromptSubmit");
  assert.match(said.additionalContext, /contract problem/);
  assert.match(said.additionalContext, /Carrick re-checked \S*users\.ts after your earlier edit to it: /);

  assert.equal((await bashHook(home, workspace.root)).stdout, "", "once: the next tool hook says nothing");
  assert.doesNotMatch((await stopWith(home)).stdout, /contract problem/, "once: the next Stop says nothing");
  assert.equal((await promptHook(home, workspace.root)).stdout, "", "once: the next prompt says nothing");
});

test("a finding that lands after Stop is printed once, by the next shell command that names no source file", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  const entry = queue(workspace.root, home, workspace.file);
  await stopWith(home);

  answer(home, entry, problem());
  const bash = await bashHook(home, workspace.root);
  assert.equal(bash.code, 0);
  const said = (JSON.parse(bash.stdout) as { hookSpecificOutput: { hookEventName: string; additionalContext: string } })
    .hookSpecificOutput;
  assert.equal(said.hookEventName, "PostToolUse");
  assert.match(said.additionalContext, /contract problem/);
  assert.equal((await promptHook(home, workspace.root)).stdout, "", "once: the next prompt says nothing");
  assert.equal((await bashHook(home, workspace.root)).stdout, "", "once: the next tool hook says nothing");
});

function searchHook(home: string, root: string, toolName: string): Promise<HookRun> {
  return runHook("post-search.ts", {
    payload: { hook_event_name: "PostToolUse", tool_name: toolName, session_id: SESSION, cwd: root, tool_input: {}, tool_response: "x" },
    env: fakeEnv({ HOME: home }),
    cwd: root,
  });
}

test("a finding that lands after Stop is printed once, by the next Grep, Glob or MCP call", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  const entry = queue(workspace.root, home, workspace.file);
  await stopWith(home);
  assert.equal((await searchHook(home, workspace.root, "Grep")).stdout, "", "nothing landed: nothing printed");

  answer(home, entry, problem());
  const grep = await searchHook(home, workspace.root, "Grep");
  assert.equal(grep.code, 0);
  const said = (JSON.parse(grep.stdout) as { hookSpecificOutput: { hookEventName: string; additionalContext: string } })
    .hookSpecificOutput;
  assert.equal(said.hookEventName, "PostToolUse");
  assert.match(said.additionalContext, /contract problem/);
  assert.equal((await searchHook(home, workspace.root, "Glob")).stdout, "", "once: the next search says nothing");
  assert.equal((await searchHook(home, workspace.root, "mcp__carrick__get_callers")).stdout, "");
  assert.equal((await promptHook(home, workspace.root)).stdout, "", "once: the next prompt says nothing");
});

test("the whole-change check Stop queued, answering later with a problem, is printed once by the next prompt", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  gapQueue(workspace.root, home, workspace.file);
  await stopWith(home);

  const [changedEntry] = changedQueued(workspace.root);
  assert.ok(changedEntry);
  const changed = parseChangedResult(fs.readFileSync(fixturePath("check-changed-problem.json"), "utf8"), 1)!;
  writeAtomic(
    path.join(sessionsDir(home), `${SESSION}.recheck.${changedEntry!.seq}.answer.json`),
    `${JSON.stringify({ ...changedEntry, state: "answered", started_at: changedEntry!.enqueued_at, finished_at: new Date().toISOString(), changed })}\n`,
  );
  const prompt = await promptHook(home, workspace.root);
  assert.match(context(prompt.stdout), /^Carrick ran `carrick check --changed` for this session's change: 1 contract problem in 1 changed file\.$/m);
  assert.equal((await promptHook(home, workspace.root)).stdout, "", "once");
});

/** An edited file whose re-check failed: a gap Stop names (a running re-check is not one). */
function gapQueue(root: string, home: string, file: string): void {
  stopped(home, queue(root, home, file));
}

/** The whole-change check answering clean, as the runner writes it: the queued entry plus its result. */
function answerChangedClean(home: string, entry: QueueEntry): void {
  const changed = parseChangedResult(fs.readFileSync(fixturePath("check-changed-clean.json"), "utf8"), 0)!;
  writeAtomic(
    path.join(sessionsDir(home), `${SESSION}.recheck.${entry.seq}.answer.json`),
    `${JSON.stringify({ ...entry, state: "answered", started_at: entry.enqueued_at, finished_at: new Date().toISOString(), changed })}\n`,
  );
}

test("a gap prints nothing at Stop and the whole-change check is still queued", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  gapQueue(workspace.root, home, workspace.file);
  assert.equal((await stopWith(home)).stdout, "", "no gap line");
  assert.equal(changedQueued(workspace.root).length, 1, "the check --changed is queued");
});

test("a clean background answer for a gap Stop did not name prints nothing on every hook event", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  gapQueue(workspace.root, home, workspace.file);
  assert.equal((await stopWith(home)).stdout, "");

  const [changedEntry] = changedQueued(workspace.root);
  answerChangedClean(home, changedEntry!);
  assert.equal((await promptHook(home, workspace.root)).stdout, "", "the next prompt says nothing");
  assert.equal((await bashHook(home, workspace.root)).stdout, "", "the next tool hook says nothing");
  assert.equal((await stopWith(home)).stdout, "", "the next Stop says nothing");
});

test("a clean whole-change check for a set Stop never named prints nothing", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  // Queued by something other than a Stop that named files (an older install's entry has no names).
  const entry = enqueueChanged(workspace.root, SESSION, Date.now(), { home })!;
  answerChangedClean(home, entry);
  assert.equal((await promptHook(home, workspace.root)).stdout, "");
});

/** The whole-change check answering with the given exit, as the runner writes it. */
function answerChangedExit(home: string, entry: QueueEntry, exit: number): void {
  const changed = parseChangedResult(fs.readFileSync(fixturePath("check-changed-clean.json"), "utf8"), exit)!;
  writeAtomic(
    path.join(sessionsDir(home), `${SESSION}.recheck.${entry.seq}.answer.json`),
    `${JSON.stringify({ ...entry, state: "answered", started_at: entry.enqueued_at, finished_at: new Date().toISOString(), changed })}\n`,
  );
}

test("a clean whole-change check for a set Stop never named prints nothing at Stop either", (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  const entry = enqueueChanged(workspace.root, SESSION, Date.now(), { home })!;
  answerChangedClean(home, entry);
  const result = stopAnswers(SESSION, { home, ensure: () => false });
  assert.doesNotMatch(result.lines.join("\n"), /found no contract problem/);
});

test("a check --changed that exits 2 prints nothing at Stop", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  gapQueue(workspace.root, home, workspace.file);
  await stopWith(home);
  const [changedEntry] = changedQueued(workspace.root);
  answerChangedExit(home, changedEntry!, 2);
  assert.equal((await stopWith(home)).stdout, "");
});

test("stop with nothing pending returns at once and names no function (carrick#3436)", async (t) => {
  const { workspace, home } = setup(t);
  // A session file an older install wrote, still listing a function it found.
  writeAtomic(
    path.join(sessionsDir(home), `${SESSION}.json`),
    `${JSON.stringify({ found: [{ name: "slugify", file: "src/a.ts", indexCommit: "abc" }], nudged: [], told: [], read: [], edited: {}, updated: new Date().toISOString() })}\n`,
  );
  const run = await stopWith(home);
  assert.ok(run.ms < SPAWNED_MS, `no wait (${run.ms} ms)`);
  assert.equal(run.stdout, "");
  void workspace;
});

test("stop says a re-check is still running, once, without waiting, and delivers the answer later", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  const entry = queue(workspace.root, home, workspace.file);

  const first = await stopWith(home);
  assert.ok(first.ms < SPAWNED_MS, `no wait (${first.ms} ms)`);
  assert.equal(first.stdout, "", "still running: Stop says nothing");

  const second = await stopWith(home, { stop_hook_active: true });
  assert.ok(second.ms < SPAWNED_MS, `no wait (${second.ms} ms)`);
  assert.equal(second.stdout, "");

  answer(home, entry, problem());
  const prompt = await promptHook(home, workspace.root);
  assert.match(context(prompt.stdout), /contract problem/);
  assert.equal((await stopWith(home)).stdout, "", "delivered once");
});

// A Stop where every gap is a re-check still running prints nothing and leaves
// the problem to the next hook event once the re-check has answered it.
test("a problem found by a re-check that was running at Stop is delivered by the next hook event, not Stop", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  const entry = queue(workspace.root, home, workspace.file);
  assert.equal((await stopWith(home)).stdout, "", "running at Stop: silent");
  answer(home, entry, problem());
  const bash = await bashHook(home, workspace.root);
  assert.match(context(bash.stdout), /contract problem/);
  assert.equal((await bashHook(home, workspace.root)).stdout, "", "once");
});

// R46.2: an answer queued in place of the probe is delivered whole or not at
// all. With nothing Stop would print for it, Stop leaves it unclaimed and does
// not count it; the next carrier prints what the probe would have in hand.
test("Stop leaves an answer queued in place of the probe with no finding unclaimed, and the next carrier prints its uses line", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  const now = Date.now();
  const entry = enqueue(workspace.root, SESSION, [workspace.file], now, { home, insteadOfProbe: true })[0]!;
  recordEdit(SESSION, workspace.root, now, home, [workspace.file]);
  assert.equal(entry.instead_of_probe, true);
  // Answered the edit, no problem, a uses line: nothing Stop prints.
  answer(home, entry, { ...clean(), uses_lines: fixture("check-stale-uses.json").uses_lines! });
  const run = await stopWith(home);
  assert.equal(run.stdout, "", "nothing for it at Stop: not unchecked, not running");
  assert.equal(fs.existsSync(path.join(sessionsDir(home), `${SESSION}.recheck.${entry.seq}.delivered`)), false, "unclaimed");
  const next = context((await bashHook(home, workspace.root)).stdout);
  assert.match(next, /Other files use what this file declares: `carrick check user-service\/src\/routes\/users\.ts` lists the 2 uses/);
});

// Ruling on R46.2: a flagged answer that did not answer the edit is a gap at
// its first Stop, named as with the switch off, once; the next carrier still
// prints what the probe would have printed in hand (its uses line).
test("Stop says nothing for a flagged answer that did not answer the edit, and the next carrier prints its uses line", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  const now = Date.now();
  const entry = enqueue(workspace.root, SESSION, [workspace.file], now, { home, insteadOfProbe: true })[0]!;
  recordEdit(SESSION, workspace.root, now, home, [workspace.file]);
  answer(home, entry, fixture("check-stale-uses.json"));
  assert.equal((await stopWith(home)).stdout, "", "a gap prints nothing");
  assert.equal((await stopWith(home)).stdout, "", "still nothing");
  const next = context((await bashHook(home, workspace.root)).stdout);
  assert.match(next, /Other files use what this file declares: `carrick check user-service\/src\/routes\/users\.ts` lists the 2 uses/);
});

// R46.2: with a finding Stop prints, Stop prints the answer as the next
// carrier would have (problem, uses and similar-function lines), once, and claims it.
test("Stop prints an answer queued in place of the probe whole when it holds a finding, and nothing prints it again", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  const now = Date.now();
  const entry = enqueue(workspace.root, SESSION, [workspace.file], now, { home, insteadOfProbe: true })[0]!;
  // Recorded, as the edit hook records every file it queues.
  recordEdit(SESSION, workspace.root, now, home, [workspace.file]);
  const lead = fixture("check-deferred-similar.json").recheck!.new_functions!;
  const whole: CheckResult = {
    ...problem(),
    recheck: { ran: "extraction+types", elapsed_ms: 1, new_functions: lead },
    uses_lines: fixture("check-stale-uses.json").uses_lines!,
  };
  answer(home, entry, whole);
  const run = await stopWith(home);
  const said = context(run.stdout);
  const once = (pattern: RegExp) =>
    assert.equal((said.match(new RegExp(pattern.source, `${pattern.flags}g`)) ?? []).length, 1, `${pattern} once in:\n${said}`);
  // The session's count, then the answer as the next carrier would print it.
  assert.ok(said.startsWith("Carrick checked the 1 file this session edited (user-service): 2 contract problems.\n\n"), said);
  once(/after your earlier edit to it: 2 contract problems\./);
  once(/^- .*type_mismatch/m);
  once(/Other files use what this file declares/);
  once(/`slugify` may already exist/);
  assert.ok(fs.existsSync(path.join(sessionsDir(home), `${SESSION}.recheck.${entry.seq}.delivered`)), "claimed at Stop");
  assert.equal((await bashHook(home, workspace.root)).stdout, "", "the next carrier prints nothing of it");
});

test("a clean late answer alone prints nothing and is marked delivered; beside a problem both print", async (t) => {
  const { workspace, home } = setup(t);
  const entry = queue(workspace.root, home, workspace.file);
  answer(home, entry, clean());
  const alone = await stopWith(home);
  assert.equal(alone.stdout, "");
  assert.ok(fs.existsSync(path.join(sessionsDir(home), `${SESSION}.recheck.${entry.seq}.delivered`)));

  const other = path.join(workspace.service, "src", "routes", "orders.ts");
  fs.writeFileSync(other, "export {};\n");
  const a = queue(workspace.root, home, workspace.file);
  const b = queue(workspace.root, home, other);
  answer(home, a, clean());
  answer(home, b, problem());
  const both = context((await stopWith(home)).stdout);
  assert.match(both, /contract problem/);
  assert.match(both, /no contract problem|re-checked/, "the clean answer rides along");
  assert.ok(both.split("\n").some((line) => line.includes("users.ts")));
  assert.ok(both.split("\n").some((line) => line.includes("orders.ts")));
});

test("a dead runner lock with queued entries: stop starts a runner before it delivers", (t) => {
  const { workspace, home } = setup(t);
  const entry = queue(workspace.root, home, workspace.file);
  const started: string[] = [];
  const result = stopAnswers(SESSION, {
    home,
    ensure: (root) => {
      started.push(root);
      answer(home, entry, problem());
      return true;
    },
  });
  assert.deepEqual(started, [workspace.root]);
  assert.ok(result.lines.some((line) => /contract problem/.test(line)));
});

test("the Stop entry carries no wait, and the prompt hook carries what lands after it", () => {
  const hooks = carrickHooks("carrick");
  const entry = hooks.Stop![0]!.hooks[0]!;
  assert.equal(entry.command, "carrick hook stop");
  assert.equal(entry.timeout, STOP_TIMEOUT_S);
  assert.equal(entry.statusMessage, undefined, "nothing to wait for, so nothing to show while waiting");
  assert.equal(hooks.UserPromptSubmit![0]!.hooks[0]!.command, "carrick hook user-prompt");
  // An ended sub-agent's late answers are owed to the main session (carrick#4074).
  assert.equal(hooks.SubagentStop![0]!.hooks[0]!.command, "carrick hook subagent-stop");
});

// A `carrick check --changed` that ran after the session's last edit has
// already said what the clause "Run `carrick check --changed` before you
// finish" asks for (carrick#3357): the agent ran it twice, 137 s then 133 s.

const CLAUSE = /If this change touches a route, a call to another service, an event or a type another service reads, run `carrick check --changed` before you finish/;

/** What `carrick check --changed` leaves in the workspace when it ends. */
function marker(root: string, startedMs: number, exit: number): void {
  writeAtomic(
    checkMarkerPath(root),
    `${JSON.stringify({ started_at_ms: startedMs, finished_at_ms: startedMs + 1000, exit })}\n`,
  );
}

/** A re-check the runner gave up on: the line that carries the clause. */
function stopped(home: string, entry: QueueEntry): void {
  const body: Answer = {
    seq: entry.seq,
    session: entry.session,
    file: entry.file,
    relative: entry.relative,
    root: entry.root,
    state: "stopped",
    enqueued_at: entry.enqueued_at,
    started_at: null,
    finished_at: new Date().toISOString(),
    failure: "the re-check ran past its budget",
  };
  writeAtomic(path.join(sessionsDir(home), `${entry.session}.recheck.${entry.seq}.answer.json`), `${JSON.stringify(body)}\n`);
}

test("stop never carries the check clause: rule 5 and the post-edit line say it (carrick#3437)", async (t) => {
  const { workspace, home } = setup(t);
  const entry = queue(workspace.root, home, workspace.file);
  recordEdit(SESSION, workspace.root, 1_000_000, home);
  // No check ran after the edit: the state in which the clause used to print.
  stopped(home, entry);
  const stoppedText = (stopAnswers(SESSION, { home })).lines.join("\n");
  assert.match(stoppedText, /was not re-checked: the re-check ran past its budget\.$/);
  assert.doesNotMatch(stoppedText, CLAUSE);

  const nothing = queue(workspace.root, home, workspace.file);
  marker(workspace.root, 2_000_000, 2);
  stopped(home, nothing);
  assert.doesNotMatch((stopAnswers(SESSION, { home })).lines.join("\n"), CLAUSE, "exit 2 checked nothing");

  liveRunner(workspace.root);
  queue(workspace.root, home, workspace.file);
  recordEdit(SESSION, workspace.root, 3_000_000, home);
  const running = (stopAnswers(SESSION, { home })).lines.join("\n");
  assert.equal(running, "", "a re-check still running prints nothing");
});

// The Stop hook states the session's contract check from the per-file
// re-checks (carrick#3466): one line when every edited file was answered after
// its last edit, else it names the file, queues one `check --changed` behind
// the re-checks and never waits for it.

/** A re-check that re-read the edited service and left no service stale. */
function reread(result: CheckResult, stale: string[] = []): CheckResult {
  return {
    ...result,
    recheck: { ran: "extraction+types", elapsed_ms: 1, scope: { reread: ["user-service"], not_reread: [], stale } },
  };
}

/** What the post-edit hook records before it probes: the file and its edit time. */
function edit(root: string, home: string, file: string, at: number): void {
  recordEdit(SESSION, root, at, home, [file]);
}

/** The `check --changed` entries waiting in the workspace's queue. */
function changedQueued(root: string): QueueEntry[] {
  const dir = queueDir(root);
  if (!fs.existsSync(dir)) return [];
  return fs
    .readdirSync(dir)
    .map((name) => JSON.parse(fs.readFileSync(path.join(dir, name), "utf8")) as QueueEntry)
    .filter((entry) => entry.kind === "changed");
}

const CHECKED = /^Carrick checked the (\d+) files? this session edited \(([^)]*)\): /;
const NOT_CHECKED = /^Carrick did not check the whole change: /; // the removed gap line: no Stop prints it

test("every edited file answered after its last edit: one checked-the-change line, no check clause, the per-answer lines folded into it", async (t) => {
  const { workspace, home } = setup(t);
  const other = path.join(workspace.service, "src", "routes", "orders.ts");
  fs.writeFileSync(other, "export {};\n");
  const at = Date.now() - 5000;
  edit(workspace.root, home, workspace.file, at);
  edit(workspace.root, home, other, at);
  const entry = queue(workspace.root, home, workspace.file);
  answer(home, entry, reread(problem()));
  // Answered in hand by the post-edit probe: no re-check was queued.
  answerInHand(workspace.root, SESSION, other, reread(clean()), Date.now(), { home });

  const result = stopAnswers(SESSION, { home, ensure: () => false });
  const text = result.lines.join("\n");
  const head = result.lines[0]!.split("\n")[0]!;
  assert.match(head, CHECKED);
  assert.equal(CHECKED.exec(head)![1], "2");
  assert.equal(CHECKED.exec(head)![2], "user-service");
  assert.match(head, /\d+ contract problems?\.$/);
  assert.doesNotMatch(text, /^Carrick re-checked/m, "the per-answer headlines are folded into the one line");
  assert.match(text, /^- \S*users\.ts:\d+:\d+ /m, "the problem lines of the answer this Stop claimed still print");
  assert.doesNotMatch(text, CLAUSE);
  assert.doesNotMatch(text, NOT_CHECKED);
  assert.equal(changedQueued(workspace.root).length, 0, "nothing to fall back on");
  assert.equal(checkedSinceLastEdit(SESSION, workspace.root, home), true, "later lines drop the clause");
});

/** A route on the edited file that the compiler compared with its consumer and found compatible. */
function judgedRoute(path: string, line: number): NonNullable<CheckResult["items"]>[number] {
  return {
    kind: "route",
    method: "GET",
    path,
    line,
    col: 1,
    source: "fact",
    counterparts: [{ role: "consumer", service: "web", file: "src/api.ts", line: 4 }],
    verdict: { state: "resolved", result: "compatible", detail: "" },
  };
}

test("a Stop over covered files with no route or call prints nothing and records the change as stated", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  answer(home, queue(workspace.root, home, workspace.file), reread(clean()));

  const first = stopAnswers(SESSION, { home, ensure: () => false });
  assert.deepEqual(first.lines, [], "no blank part either");
  assert.equal(checkedSinceLastEdit(SESSION, workspace.root, home), true, "the change is still settled");
  assert.ok(readSession(SESSION, home).stated[workspace.root]! >= readSession(SESSION, home).edited[workspace.root]!);

  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  answer(home, queue(workspace.root, home, workspace.file), reread(clean()));
  assert.deepEqual((stopAnswers(SESSION, { home, ensure: () => false })).lines, []);
});

test("a Stop over 2 judged operations with no problem names both and says both sides agree", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  const judged = reread(clean());
  judged.items = [judgedRoute("/api/users", 10), { ...judgedRoute("/orders", 40), kind: "call", method: "POST" }];
  answer(home, queue(workspace.root, home, workspace.file), judged);

  const first = stopAnswers(SESSION, { home, ensure: () => false });
  assert.equal(first.lines.length, 1);
  assert.equal(
    first.lines[0],
    "Carrick checked 2 operations in the file edited this session (GET /api/users at user-service/src/routes/users.ts:10; a call to POST /orders at user-service/src/routes/users.ts:40): both sides agree.",
  );
  assert.equal(changedQueued(workspace.root).length, 0);
});

test("a second Stop over the same operations prints nothing, a new operation prints again", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  const judged = reread(clean());
  judged.items = [judgedRoute("/api/users", 10)];
  answer(home, queue(workspace.root, home, workspace.file), judged);
  assert.equal((stopAnswers(SESSION, { home, ensure: () => false })).lines.length, 1);

  // Edited again (the route moved down a line), answered with the same operation.
  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  const moved = reread(clean());
  moved.items = [judgedRoute("/api/users", 12)];
  answer(home, queue(workspace.root, home, workspace.file), moved);
  assert.deepEqual((stopAnswers(SESSION, { home, ensure: () => false })).lines, []);

  // A new operation is a new set.
  edit(workspace.root, home, workspace.file, Date.now() - 200);
  const more = reread(clean());
  more.items = [judgedRoute("/api/users", 12), judgedRoute("/api/orders", 30)];
  answer(home, queue(workspace.root, home, workspace.file), more);
  const third = stopAnswers(SESSION, { home, ensure: () => false });
  assert.match(third.lines.join("\n"), /^Carrick checked 2 operations in the file edited this session/);
});

/** A clean answer that judged one operation of `relative` (a route at `line`). */
function cleanJudged(relative: string, routePath: string, line: number): CheckResult {
  return { ...clean(), file: relative, items: [judgedRoute(routePath, line)], recheck: { ran: "extraction+types", elapsed_ms: 1 } };
}

/** A second TS file in the workspace's service, beside the edited one. */
function otherFile(workspace: { service: string }): string {
  const other = path.join(workspace.service, "src", "routes", "orders.ts");
  fs.writeFileSync(other, "export {};\n");
  return other;
}

// R51.1: a flagged answer that answered the edit counts at Stop exactly as an
// unflagged one, whether or not a carrier has already claimed it.
test("Stop counts two flagged clean answers as 2 operations when a carrier already delivered the first", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  const other = otherFile(workspace);
  const first = Date.now() - 5000;
  const a = enqueue(workspace.root, SESSION, [workspace.file], first, { home, insteadOfProbe: true })[0]!;
  recordEdit(SESSION, workspace.root, first, home, [workspace.file]);
  answer(home, a, cleanJudged("src/routes/users.ts", "/api/users", 10));
  await bashHook(home, workspace.root);
  assert.ok(fs.existsSync(path.join(sessionsDir(home), `${SESSION}.recheck.${a.seq}.delivered`)), "the carrier delivered the first");
  const second = Date.now();
  const b = enqueue(workspace.root, SESSION, [other], second, { home, insteadOfProbe: true })[0]!;
  recordEdit(SESSION, workspace.root, second, home, [other]);
  answer(home, b, cleanJudged("src/routes/orders.ts", "/api/orders", 5));
  const said = stopAnswers(SESSION, { home, ensure: () => false }).lines.join("\n\n");
  assert.match(said, /^Carrick checked 2 operations in files edited this session \(.*\): both sides agree\.$/, said);
});

test("Stop counts a flagged finding and a flagged clean answer as the 2 files this session edited", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  const other = otherFile(workspace);
  const now = Date.now();
  const [a, b] = enqueue(workspace.root, SESSION, [workspace.file, other], now, { home, insteadOfProbe: true });
  recordEdit(SESSION, workspace.root, now, home, [workspace.file, other]);
  answer(home, a!, problem());
  answer(home, b!, cleanJudged("src/routes/orders.ts", "/api/orders", 5));
  const said = stopAnswers(SESSION, { home, ensure: () => false }).lines.join("\n\n");
  assert.ok(said.startsWith("Carrick checked the 2 files this session edited (user-service): "), said);
});

test("Stop says both sides agree for one flagged clean answer with a judged operation, and the next carrier still prints its uses line once", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  const now = Date.now();
  const entry = enqueue(workspace.root, SESSION, [workspace.file], now, { home, insteadOfProbe: true })[0]!;
  recordEdit(SESSION, workspace.root, now, home, [workspace.file]);
  answer(home, entry, { ...cleanJudged("src/routes/users.ts", "/api/users", 10), uses_lines: fixture("check-stale-uses.json").uses_lines! });
  const stop = stopAnswers(SESSION, { home, ensure: () => false }).lines.join("\n\n");
  assert.equal(
    stop,
    "Carrick checked 1 operation in the file edited this session (GET /api/users at user-service/src/routes/users.ts:10): both sides agree.",
  );
  const next = context((await bashHook(home, workspace.root)).stdout);
  assert.match(next, /Other files use what this file declares: `carrick check user-service\/src\/routes\/users\.ts` lists the 2 uses/);
  assert.equal((await bashHook(home, workspace.root)).stdout, "", "once");
});

test("a Stop whose only gap is a file no service reads prints nothing and queues nothing", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  const unread: CheckResult = {
    ...fixture("check-deferred.json"),
    recheck: { ran: "none", elapsed_ms: 0, reason: "no service reads this file" },
  };
  answer(home, queue(workspace.root, home, workspace.file), unread);

  const result = stopAnswers(SESSION, { home, ensure: () => false });
  assert.deepEqual(result.lines, []);
  assert.equal(changedQueued(workspace.root).length, 0);
});

test("a Stop whose only gap is an edit that changed nothing the scan reads prints nothing and queues nothing", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  const comment: CheckResult = {
    ...fixture("check-deferred.json"),
    recheck: {
      ran: "none",
      elapsed_ms: 0,
      reason: "the edit changed nothing the scan reads (comments or layout); verdicts are the index's",
    },
  };
  answer(home, queue(workspace.root, home, workspace.file), comment);

  const result = stopAnswers(SESSION, { home, ensure: () => false });
  assert.deepEqual(result.lines, []);
  assert.equal(changedQueued(workspace.root).length, 0);
});

test("a Stop whose answer holds only unread calls prints no line for them (carrick#3932)", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  const unread = reread(clean());
  unread.unread = [3, 9].map((line) => ({
    file: workspace.file,
    line,
    reason: "value_callee",
    phrase: "Carrick can't tell what kind of object this is",
    edited: true,
  }));
  answer(home, queue(workspace.root, home, workspace.file), unread);
  const text = stopAnswers(SESSION, { home, ensure: () => false }).lines.join("\n");
  assert.ok(!text.includes("could not read the call"));
});

test("a Stop with 1 problem prints the header and its line", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  answer(home, queue(workspace.root, home, workspace.file), reread(problem()));

  const lines = (stopAnswers(SESSION, { home, ensure: () => false })).lines.join("\n").split("\n");
  assert.match(lines[0]!, CHECKED);
  assert.match(lines[0]!, /\d+ contract problems?\.$/);
  assert.match(lines[1]!, /^- \S*users\.ts:\d+:\d+ /);
});

test("an edit after the answer is not covered: Stop says nothing and queues check --changed", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  answer(home, queue(workspace.root, home, workspace.file), reread(clean()));
  edit(workspace.root, home, workspace.file, Date.now() + 5000);

  const text = (stopAnswers(SESSION, { home, ensure: () => false })).lines.join("\n");
  assert.doesNotMatch(text, CHECKED);
  assert.equal(text, "", "a gap prints nothing");
  assert.equal(changedQueued(workspace.root).length, 1);
});

test("a Stop over a pending re-check prints nothing and queues no check --changed", (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  queue(workspace.root, home, workspace.file);
  const first = stopAnswers(SESSION, { home, ensure: () => false });
  assert.deepEqual(first.lines, [], "still running: nothing at Stop");
  assert.equal(changedQueued(workspace.root).length, 0, "the running re-check delivers its own answer: no check --changed");
  const second = stopAnswers(SESSION, { home, ensure: () => false });
  assert.deepEqual(second.lines, []);
});

for (const [name, arrange, reason] of [
  [
    "an answer with a stale service",
    (root: string, home: string, file: string) => answer(home, queue(root, home, file), reread(clean(), ["billing"])),
    /1 service connected to it by import or operation was judged at its indexed version \(billing\)/,
  ],
  [
    "a deferred answer",
    (root: string, home: string, file: string) => answer(home, queue(root, home, file), fixture("check-deferred.json")),
    /this edit needs a re-scan, which this call was asked not to start/,
  ],
  [
    "a failed answer",
    (root: string, home: string, file: string) => stopped(home, queue(root, home, file)),
    /the re-check ran past its budget/,
  ],
] as const) {
  test(`${name} makes Stop name the file, print no checked line, and queue one changed entry it does not wait for`, (t) => {
    const { workspace, home } = setup(t);
    edit(workspace.root, home, workspace.file, Date.now() - 5000);
    arrange(workspace.root, home, workspace.file);
    const ensured: string[] = [];
    const result = stopAnswers(SESSION, {
      home,
      ensure: (root) => {
        ensured.push(root);
        return false;
      },
    });
    const text = result.lines.join("\n");
    assert.doesNotMatch(text, CHECKED);
    assert.equal(text, "", "a gap prints nothing");
    assert.doesNotMatch(text, CLAUSE);
    assert.equal(changedQueued(workspace.root).length, 1);
    assert.ok(ensured.includes(workspace.root), "a runner is made sure of for the queued check");

    // The change is not stated twice.
    const second = stopAnswers(SESSION, { home, ensure: () => false });
    assert.doesNotMatch(second.lines.join("\n"), NOT_CHECKED);
    assert.equal(changedQueued(workspace.root).length, 1, "one changed entry, not one per Stop");
  });
}

/** The background check --changed answers, as the runner writes it. */
function answerChanged(home: string, entry: QueueEntry): void {
  const changed = parseChangedResult(fs.readFileSync(fixturePath("check-changed-clean.json"), "utf8"), 0)!;
  writeAtomic(
    path.join(sessionsDir(home), `${SESSION}.recheck.${entry.seq}.answer.json`),
    `${JSON.stringify({ ...entry, state: "answered", started_at: entry.enqueued_at, finished_at: new Date().toISOString(), changed })}\n`,
  );
}

test("a gap prints nothing, and the answered whole-change check queues no second one until the file changes again (carrick#3543)", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  answer(home, queue(workspace.root, home, workspace.file), fixture("check-deferred.json"));

  const first = (stopAnswers(SESSION, { home, ensure: () => false })).lines.join("\n");
  assert.equal(first, "", "a gap prints nothing");
  const [queued] = changedQueued(workspace.root);
  assert.ok(queued);
  answerChanged(home, queued!);

  // The agent keeps editing another file: the answered gap is not named again.
  const other = path.join(workspace.service, "src", "routes", "orders.ts");
  fs.writeFileSync(other, "export {};\n");
  edit(workspace.root, home, other, Date.now() + 1000);
  answerInHand(workspace.root, SESSION, other, reread(clean()), Date.now() + 2000, { home });
  const second = (stopAnswers(SESSION, { home, ensure: () => false })).lines.join("\n");
  assert.doesNotMatch(second, NOT_CHECKED);
  // Nothing names the file.
  assert.doesNotMatch(second, /users\.ts/);

  // Nothing new: nothing said.
  const third = stopAnswers(SESSION, { home, ensure: () => false });
  assert.doesNotMatch(third.lines.join("\n"), NOT_CHECKED);

  // The file changes again: it is named again.
  edit(workspace.root, home, workspace.file, Date.now() + 60_000);
  const fourth = (stopAnswers(SESSION, { home, ensure: () => false })).lines.join("\n");
  assert.equal(fourth, "", "a gap prints nothing");
});

/** A linked git worktree under the workspace (its `.git` is a file) holding one edited file. */
function linkedWorktree(root: string, name: string): { top: string; file: string } {
  const top = path.join(root, ".claude", "worktrees", name);
  const file = path.join(top, "user-service", "src", "a.ts");
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, "export {};\n");
  fs.writeFileSync(path.join(top, ".git"), "gitdir: /elsewhere/.git/worktrees/x\n");
  return { top, file };
}

test("a file edited in a worktree that was removed is not named at Stop (carrick#3595)", async (t) => {
  const { workspace, home } = setup(t);
  const { top, file } = linkedWorktree(workspace.root, "gone");
  edit(workspace.root, home, file, Date.now() - 5000);
  fs.rmSync(top, { recursive: true, force: true });

  const result = stopAnswers(SESSION, { home, ensure: () => false });
  assert.doesNotMatch(result.lines.join("\n"), /worktrees\/gone|a\.ts/, "the removed worktree's file is not named");
  assert.equal(changedQueued(workspace.root).length, 0, "no whole-change check is queued for it");
});

test("a file edited in a worktree that still exists still queues a check --changed (carrick#3595)", async (t) => {
  const { workspace, home } = setup(t);
  const { file } = linkedWorktree(workspace.root, "live");
  edit(workspace.root, home, file, Date.now() - 5000);

  const text = (stopAnswers(SESSION, { home, ensure: () => false })).lines.join("\n");
  assert.equal(text, "", "a gap prints nothing");
  assert.equal(changedQueued(workspace.root).length, 1);
});

test("a file deleted in the main checkout still queues a check --changed: only a gone worktree drops an entry (carrick#3595)", async (t) => {
  const { workspace, home } = setup(t);
  fs.mkdirSync(path.join(workspace.root, ".git"));
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  fs.rmSync(workspace.file);

  const text = (stopAnswers(SESSION, { home, ensure: () => false })).lines.join("\n");
  assert.equal(text, "", "a gap prints nothing");
  assert.equal(changedQueued(workspace.root).length, 1);
});

test("stop delivers a finished check --changed answer: its verdict, never a clause", async (t) => {
  const { workspace, home } = setup(t);
  const entry = enqueueChanged(workspace.root, SESSION, Date.now(), { home })!;
  const changed = parseChangedResult(
    fs.readFileSync(fixturePath("check-changed-problem.json"), "utf8"),
    1,
  )!;
  writeAtomic(
    path.join(sessionsDir(home), `${SESSION}.recheck.${entry.seq}.answer.json`),
    `${JSON.stringify({ ...entry, state: "answered", started_at: entry.enqueued_at, finished_at: new Date().toISOString(), changed })}\n`,
  );
  const text = (stopAnswers(SESSION, { home, ensure: () => false })).lines.join("\n");
  assert.match(text, /^Carrick ran `carrick check --changed` for this session's change: 1 contract problem in 1 changed file\.$/m);
  assert.match(text, /^- order-service\/src\/client\.ts:14 call GET \/users\/:id: the response type does not match: email is missing/m);
  assert.doesNotMatch(text, CLAUSE);
});

// --- carrick#4074: an ended sub-agent's late answers reach the main session once ---
//
// A sub-agent's re-checks are filed under `<session>.<agent>`. Once it has
// ended, none of its hooks runs again, so an answer landing after that was
// never read. `carrick hook subagent-stop` marks the end; the main session's
// readers (the carriers and Stop) then deliver what the ended agent is owed,
// findings only.

const AGENT = "agent-x";
const SUB = `${SESSION}.${AGENT}`;

function subagentStop(home: string, payload: Record<string, unknown> = { agent_id: AGENT }): Promise<HookRun> {
  return runHook("subagent-stop.ts", {
    payload: { hook_event_name: "SubagentStop", session_id: SESSION, ...payload },
    env: fakeEnv({ HOME: home }),
  });
}

function endedMarkers(home: string): string[] {
  try {
    return fs.readdirSync(sessionsDir(home)).filter((name) => name.endsWith(".ended"));
  } catch {
    return [];
  }
}

/** A tool hook of the sub-agent itself: a Read with no file, which only carries late answers. */
function subagentCarrier(home: string, root: string, agent = AGENT): Promise<HookRun> {
  return runHook("post-read.ts", {
    payload: { hook_event_name: "PostToolUse", tool_name: "Read", session_id: SESSION, agent_id: agent, cwd: root, tool_input: {} },
    env: fakeEnv({ HOME: home }),
  });
}

function said(run: HookRun): string {
  return run.stdout ? (JSON.parse(run.stdout) as { hookSpecificOutput: { additionalContext: string } }).hookSpecificOutput.additionalContext : "";
}

test("SubagentStop prints nothing and writes the marker; a payload with no agent_id writes nothing", async (t) => {
  const { home } = setup(t);
  const run = await subagentStop(home);
  assert.equal(run.code, 0);
  assert.equal(run.stdout, "");
  assert.deepEqual(endedMarkers(home), [`${SUB}.ended`]);
  assert.equal(sessionRecords(home), 0, "the marker is not a session record");

  const other = setup(t).home;
  const none = await subagentStop(other, {});
  assert.equal(none.code, 0);
  assert.equal(none.stdout, "");
  assert.deepEqual(endedMarkers(other), []);
});

test("a sub-agent's answer that lands after its SubagentStop is delivered once by the main session's next prompt, and the next tool hook says nothing", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  const entry = queue(workspace.root, home, workspace.file, SUB);
  await subagentStop(home);
  answer(home, entry, problem());

  const prompt = await promptHook(home, workspace.root);
  const text = (JSON.parse(prompt.stdout) as { hookSpecificOutput: { additionalContext: string } }).hookSpecificOutput.additionalContext;
  assert.match(text, /contract problem/);
  assert.match(text, /users\.ts/);
  assert.equal((await bashHook(home, workspace.root)).stdout, "", "once: the next tool hook says nothing");
  assert.equal((await promptHook(home, workspace.root)).stdout, "", "once: the next prompt says nothing");
});

test("the main Stop carries an ended sub-agent's problem answer, once, beside the main session's own clean line", async (t) => {
  const { workspace, home } = setup(t);
  const other = path.join(workspace.service, "src", "routes", "orders.ts");
  fs.writeFileSync(other, "export {};\n");
  // The main session's own edit: covered, clean, not yet stated.
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  const judged = reread(clean());
  judged.items = [judgedRoute("/api/users", 10)];
  answer(home, queue(workspace.root, home, workspace.file), judged);
  // The sub-agent's edit of another file, answered with a problem after it ended.
  const subEntry = queue(workspace.root, home, other, SUB);
  await subagentStop(home);
  answer(home, subEntry, reread(problem()));

  const first = stopAnswers(SESSION, { home, ensure: () => false }).lines;
  const text = first.join("\n");
  assert.match(first[0]!, /^Carrick checked 1 operation in the file edited this session \(GET \/api\/users at user-service\/src\/routes\/users\.ts:10\)/);
  assert.doesNotMatch(first[0]!, /orders\.ts/, "the sub-agent's file is not the session's own edit");
  assert.match(text, /orders\.ts/);
  assert.match(text, /contract problem/);
  assert.equal(text.split("\n").filter((line) => /contract problem/.test(line)).length, 1);
  assert.deepEqual(stopAnswers(SESSION, { home, ensure: () => false }).lines, [], "once: the next Stop says nothing");
});

test("an answer for a still-running sub-agent (no SubagentStop) is delivered to that sub-agent, not the main session", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  const entry = queue(workspace.root, home, workspace.file, SUB);
  answer(home, entry, problem());

  assert.equal((await promptHook(home, workspace.root)).stdout, "");
  assert.deepEqual(stopAnswers(SESSION, { home, ensure: () => false }).lines, []);
  assert.match(said(await subagentCarrier(home, workspace.root)), /contract problem/);
});

test("a sub-agent resumed after SubagentStop: its next hook re-opens it, and an answer landing then goes to it", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  await subagentStop(home);
  assert.deepEqual(endedMarkers(home), [`${SUB}.ended`]);

  assert.equal((await subagentCarrier(home, workspace.root)).stdout, "", "nothing owed yet");
  assert.deepEqual(endedMarkers(home), [], "the sub-agent's own hook deleted the marker");

  const entry = queue(workspace.root, home, workspace.file, SUB);
  answer(home, entry, problem());
  assert.equal((await promptHook(home, workspace.root)).stdout, "", "the main session adopts nothing for a resumed sub-agent");
  assert.match(said(await subagentCarrier(home, workspace.root)), /contract problem/);
});

test("an answer the main session adopted is not delivered again to the resumed sub-agent", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  const entry = queue(workspace.root, home, workspace.file, SUB);
  await subagentStop(home);
  answer(home, entry, problem());
  assert.match(said(await promptHook(home, workspace.root)), /contract problem/);

  assert.equal(said(await subagentCarrier(home, workspace.root)), "", "claimed once, under its owner's key");
});

// A1 (carrick#4655, T4): an answer whose only mismatch the re-check's judge
// did not state (it was on main) holds no finding: the late carrier and a
// parent adopting it say nothing.
test("T4: an answer whose only problem the judge did not state has no finding and no adopted line", () => {
  const onMain: Answer = {
    seq: "1",
    session: "s",
    file: "/w/src/routes/users.ts",
    relative: "src/routes/users.ts",
    root: "/w",
    state: "answered",
    enqueued_at: new Date().toISOString(),
    started_at: null,
    finished_at: new Date().toISOString(),
    result: { ...fixture("check-mismatch.json"), recheck: { ran: "extraction+types", elapsed_ms: 1 } },
  };
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-t4-"));
  assert.equal(hasFindings(onMain), false);
  assert.equal(adoptedLine(onMain, home), null);
  const stated: Answer = { ...onMain, result: problem() };
  assert.equal(hasFindings(stated), true);
  assert.match(adoptedLine(stated, home) ?? "", /contract problem/);
});

// A1 (D1): the `--changed` line states what a plain `check --changed` lists,
// the per-file rule, including a break resting on another build's index (exit 2).
test("the --changed line states the listed problems, on exit 1 and on exit 2", () => {
  const base = parseChangedResult(fs.readFileSync(fixturePath("check-changed-problem.json"), "utf8"), 1)!;
  assert.match(changedLines(base), /: 1 contract problem in 1 changed file\./);
  const otherBuild = { ...base, exit: 2, failing: 0, problems: base.problems.map((p) => ({ ...p, fails: false, other_build: true })) };
  assert.match(changedLines(otherBuild), /: 1 contract problem in 1 changed file\./);
  const unlisted = { ...base, problems: base.problems.map((p) => ({ ...p, stated: false })) };
  assert.equal(changedLines(unlisted), "");
});

// --- an early break counts at Stop only while it still stands (T1, carrick#4728) ---

/** The break the held compiler states for GET /api/zz, whose route is on the workspace's file. */
function zzBreak(file: string, over: Partial<HeldLine> = {}): HeldLine {
  return {
    row_key: "contract::user-service::src/routes/users.ts::producer::GET /api/zz::0",
    group: 0,
    label: "GET /api/zz",
    consumer: "order-service",
    file: "src/routes/users.ts",
    line: 12,
    message: "Type '{ name: string; }' is not assignable to type 'O1_0'.",
    row_file: "src/routes/users.ts",
    row_line: 10,
    row_path: file,
    places: [{ service: "order-service", file: "src/clients/users.ts", line: 4 }],
    expected: "{ id: string }",
    main_commit: null,
    named_files: [],
    run: "e1.1",
    ...over,
  };
}

/** An edit of `file` whose early answer lists `lines` and whose part 2 answered clean. */
function editWithEarly(root: string, home: string, file: string, lines: HeldLine[]): void {
  edit(root, home, file, Date.now() - 1000);
  const entry = queue(root, home, file);
  assert.ok(answerEarly(SESSION, entry.seq, { state: "ready", lines, ms: 3, service: "user-service/user-service", engine: "e1" }, Date.now(), home));
  answer(home, entry, reread(clean()));
}

const zzLines = (text: string): number => text.split("\n").filter((line) => line.includes("GET /api/zz") && line.includes("type_mismatch")).length;

test("T1: an early break, then a comment edit of the same file: Stop counts it once", (t) => {
  const { workspace, home } = setup(t);
  editWithEarly(workspace.root, home, workspace.file, [zzBreak(workspace.file)]);
  editWithEarly(workspace.root, home, workspace.file, [zzBreak(workspace.file, { standing: true })]);
  const head = stopAnswers(SESSION, { home, ensure: () => false }).lines.join("\n").split("\n")[0]!;
  assert.match(head, CHECKED);
  assert.match(head, /: 1 contract problem\.$/, head);
});

test("T1: an early break fixed in another file: Stop counts none and nothing prints it", (t) => {
  const { workspace, home } = setup(t);
  const other = path.join(workspace.service, "src", "routes", "orders.ts");
  fs.writeFileSync(other, "export {};\n");
  editWithEarly(workspace.root, home, workspace.file, [zzBreak(workspace.file)]);
  editWithEarly(workspace.root, home, other, []);
  const text = stopAnswers(SESSION, { home, ensure: () => false }).lines.join("\n");
  assert.equal(zzLines(text), 0, text);
  assert.doesNotMatch(text, /contract problem/, text);
});

test("T1: break, fix, break again: the break prints twice", (t) => {
  const { workspace, home } = setup(t);
  const texts: string[] = [];
  editWithEarly(workspace.root, home, workspace.file, [zzBreak(workspace.file)]);
  texts.push(...deliverLate(SESSION, Date.now(), new Set(), home));
  editWithEarly(workspace.root, home, workspace.file, []);
  texts.push(...deliverLate(SESSION, Date.now(), new Set(), home));
  editWithEarly(workspace.root, home, workspace.file, [zzBreak(workspace.file, { run: "e1.2" })]);
  texts.push(...deliverLate(SESSION, Date.now(), new Set(), home));
  assert.equal(zzLines(texts.join("\n")), 2, texts.join("\n"));
});

// --- Stop never contradicts or miscounts a stated early break (T4; carrick#4703, #4786, #4900) ---

const yyBreak = (file: string, over: Partial<HeldLine> = {}): HeldLine =>
  zzBreak(file, { row_key: "contract::user-service::src/routes/users.ts::producer::GET /api/yy::0", label: "GET /api/yy", row_path: file, ...over });

/** A route of the edited file whose part-2 verdict was not judged: the file's own calls are `not_checked`. */
function unjudgedRoute(routePath: string, line: number): NonNullable<CheckResult["items"]>[number] {
  return { ...judgedRoute(routePath, line), counterparts: [], verdict: { state: "unresolved", detail: "" } as never };
}

/** An edit whose early answer lists `lines` (from `engine`) and whose part 2 answered `full`. */
function editWith(root: string, home: string, file: string, lines: HeldLine[], full: CheckResult, engine = "e1"): void {
  edit(root, home, file, Date.now() - 1000);
  const entry = queue(root, home, file);
  assert.ok(answerEarly(SESSION, entry.seq, { state: "ready", lines, ms: 3, service: "user-service/user-service", engine }, Date.now(), home));
  answer(home, entry, reread(full));
}

test("T4 step 0 (CF1 shape): 2 early breaks, part 2 judged neither: Stop counts 2 and names no missing verdict", (t) => {
  const { workspace, home } = setup(t);
  const full = { ...clean(), file: "src/routes/users.ts", items: [unjudgedRoute("/api/zz", 10), unjudgedRoute("/api/yy", 20)] };
  editWith(workspace.root, home, workspace.file, [zzBreak(workspace.file), yyBreak(workspace.file)], full);
  const text = stopAnswers(SESSION, { home, ensure: () => false }).lines.join("\n");
  assert.match(text.split("\n")[0]!, /: 2 contract problems\.$/, text);
  assert.doesNotMatch(text, /no contract verdict|found no contract problem/, text);
});

test("T4 P4.4 (#4900): one break listed by two files' early answers is counted once", (t) => {
  const { workspace, home } = setup(t);
  const other = otherFile(workspace);
  editWith(workspace.root, home, workspace.file, [zzBreak(workspace.file)], clean());
  editWith(workspace.root, home, other, [zzBreak(workspace.file, { standing: true })], clean());
  const text = stopAnswers(SESSION, { home, ensure: () => false }).lines.join("\n");
  assert.match(text.split("\n")[0]!, /: 1 contract problem\.$/, text);
});

test("T4 P4.6: an early break whose named file the session edited: Stop's line says main's copy was read and the file changed", (t) => {
  const { workspace, home } = setup(t);
  editWith(workspace.root, home, workspace.file, [zzBreak(workspace.file, { main_commit: "abcdef1234567890", named_files: [workspace.file] })], clean());
  const text = stopAnswers(SESSION, { home, ensure: () => false }).lines.join("\n");
  assert.match(text, /The caller's side was read from main at abcdef1\. You changed user-service\/src\/routes\/users\.ts in this session; the check used main's copy of it\./, text);
});

test("T4 P4.6: an early break with no session edit of a named file: the read-from-main line alone", (t) => {
  const { workspace, home } = setup(t);
  editWith(workspace.root, home, workspace.file, [zzBreak(workspace.file, { main_commit: "abcdef1234567890", named_files: [] })], clean());
  const text = stopAnswers(SESSION, { home, ensure: () => false }).lines.join("\n");
  assert.match(text, /The caller's side was read from main at abcdef1\.$/m, text);
  assert.doesNotMatch(text, /You changed/, text);
});

test("T4 P4.6: a part-2 item carries no copy line", (t) => {
  const { workspace, home } = setup(t);
  editWith(workspace.root, home, workspace.file, [], problem());
  const text = stopAnswers(SESSION, { home, ensure: () => false }).lines.join("\n");
  assert.match(text, /type_mismatch/, text);
  assert.doesNotMatch(text, /read from main/, text);
});

test("T4 P4.6: a break whose named caller file the session edited is not counted and the file gets no clean line", (t) => {
  const { workspace, home } = setup(t);
  editWith(workspace.root, home, workspace.file, [zzBreak(workspace.file, { main_commit: "abcdef1234567890", named_files: [workspace.file] })], clean());
  const text = stopAnswers(SESSION, { home, ensure: () => false }).lines.join("\n");
  assert.doesNotMatch(text, /Carrick checked the .*contract problem/, text);
  assert.doesNotMatch(text, /found no contract problem|both sides agree/, text);
  assert.match(text, /You changed user-service\/src\/routes\/users\.ts in this session/, text);
});

test("T4 P4.6: the same break with no session edit of a named file is counted once", (t) => {
  const { workspace, home } = setup(t);
  editWith(workspace.root, home, workspace.file, [zzBreak(workspace.file, { main_commit: "abcdef1234567890", named_files: [] })], clean());
  const text = stopAnswers(SESSION, { home, ensure: () => false }).lines.join("\n");
  assert.match(text.split("\n")[0]!, /: 1 contract problem\.$/, text);
});

test("T4 P4.5: the newest early answer is another engine's: Stop counts no break and says no clean line for the file", (t) => {
  const { workspace, home } = setup(t);
  const full = { ...clean(), file: "src/routes/users.ts", items: [unjudgedRoute("/api/zz", 10)] };
  editWith(workspace.root, home, workspace.file, [zzBreak(workspace.file)], full, "e1");
  editWith(workspace.root, home, workspace.file, [], full, "e2");
  const text = stopAnswers(SESSION, { home, ensure: () => false }).lines.join("\n");
  assert.doesNotMatch(text, /contract problem|no contract verdict|both sides agree/, text);
});

test("T4 P4.5: a late carrier does not print a break the newest answer of another engine does not list", (t) => {
  const { workspace, home } = setup(t);
  editWith(workspace.root, home, workspace.file, [zzBreak(workspace.file)], clean(), "e1");
  const other = otherFile(workspace);
  editWith(workspace.root, home, other, [], clean(), "e2");
  assert.equal(zzLines(deliverLate(SESSION, Date.now(), new Set(), home).join("\n")), 0);
});

test("T4 P4.2 (#4786): a clean check --changed for a file whose early break stands prints no clean line", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  editWith(workspace.root, home, workspace.file, [zzBreak(workspace.file)], clean());
  edit(workspace.root, home, workspace.file, Date.now());
  gapQueue(workspace.root, home, workspace.file);
  await stopWith(home);
  const [changedEntry] = changedQueued(workspace.root);
  answerChangedClean(home, changedEntry!);
  const text = stopAnswers(SESSION, { home, ensure: () => false }).lines.join("\n");
  assert.doesNotMatch(text, /found no contract problem/, text);
});

// A Stop that saw every re-check still running records the change as waiting,
// not stated: the carrier that later delivers the answer prints nothing for a
// re-check that ran out of time, so the next Stop names that gap.

/** A re-check that gave up before it read the edited service, as the CLI writes it. */
function overran(reason: string, over: Record<string, unknown> = {}): CheckResult {
  return {
    ...clean(),
    stale: true,
    recheck: { ran: "none", elapsed_ms: 180_000, reason, ...over },
  } as CheckResult;
}

test("a gap that lands after a Stop saw it running prints nothing at the next Stop", (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  const first = queue(workspace.root, home, workspace.file);
  const second = queue(workspace.root, home, workspace.file);
  assert.deepEqual(stopAnswers(SESSION, { home, ensure: () => false }).lines, [], "running: nothing");

  answer(home, first, overran("the re-scan ran past the budget", { deadline: true }));
  answer(home, second, overran("re-reading shop-api (74 indexed files) took longer than 180 s against this index, so edits in shop-api are not re-checked until `carrick refresh`.", { known_miss: true }));
  const lines = stopAnswers(SESSION, { home, ensure: () => false }).lines;
  assert.deepEqual(lines, [], "a gap prints nothing");
  assert.deepEqual(stopAnswers(SESSION, { home, ensure: () => false }).lines, [], "still nothing");
});

test("a finding a carrier already printed is not restated by the Stop after a running one", (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  const entry = queue(workspace.root, home, workspace.file);
  assert.deepEqual(stopAnswers(SESSION, { home, ensure: () => false }).lines, [], "running: nothing");
  answer(home, entry, problem());
  assert.ok(deliverLate(SESSION, Date.now(), new Set(), home).length > 0, "the carrier prints the finding");
  assert.deepEqual(stopAnswers(SESSION, { home, ensure: () => false }).lines, [], "not restated");
});

test("a Stop that still sees the re-check running keeps waiting and says nothing", (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  const entry = queue(workspace.root, home, workspace.file);
  assert.deepEqual(stopAnswers(SESSION, { home, ensure: () => false }).lines, []);
  assert.deepEqual(stopAnswers(SESSION, { home, ensure: () => false }).lines, []);
  answer(home, entry, overran("re-reading shop-api took longer than 180 s", { known_miss: true }));
  assert.deepEqual(stopAnswers(SESSION, { home, ensure: () => false }).lines, [], "a gap that lands prints nothing");
  assert.equal(changedQueued(workspace.root).length, 1);
});
