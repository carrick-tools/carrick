// The PostToolUse, SessionStart and Stop hooks, run as Claude Code runs them.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { editPayload, fakeEnv, firstCall, fixturePath, makeWorkspace, runHook } from "./helpers.ts";

test("an edit gets the verdicts as additionalContext", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  const run = await runHook("post-edit.ts", { payload: editPayload(workspace) });
  assert.equal(run.code, 0);
  const parsed = JSON.parse(run.stdout) as {
    hookSpecificOutput: { hookEventName: string; additionalContext: string };
  };
  assert.equal(parsed.hookSpecificOutput.hookEventName, "PostToolUse");
  assert.match(
    parsed.hookSpecificOutput.additionalContext,
    /^Carrick checked user-service\/src\/routes\/users\.ts against the workspace index/,
  );
  assert.match(parsed.hookSpecificOutput.additionalContext, /type_mismatch/);
});

test("MultiEdit payloads name their file the other way round", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  const run = await runHook("post-edit.ts", {
    payload: {
      tool_name: "MultiEdit",
      cwd: workspace.root,
      tool_input: { edits: [{ file_path: workspace.file }] },
    },
  });
  assert.equal(run.code, 0);
  assert.match(run.stdout, /additionalContext/);
});

test("a file with no indexed rows still gets the boundary", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  const clean = await runHook("post-edit.ts", {
    payload: editPayload(workspace),
    env: fakeEnv({ CARRICK_FAKE_FIXTURE: fixturePath("check-clean.json") }),
  });
  assert.equal(clean.code, 0);
  const context = (
    JSON.parse(clean.stdout) as { hookSpecificOutput: { additionalContext: string } }
  ).hookSpecificOutput.additionalContext;
  assert.match(context, /A local index holds what the deterministic passes state/);
});

test("the hook is silent when there is nothing to say", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  const silent = await runHook("post-edit.ts", {
    payload: editPayload(workspace),
    env: fakeEnv({ CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json") }),
  });
  assert.equal(silent.stdout, "");
  assert.equal(silent.code, 0);

  const notIndexed = await runHook("post-edit.ts", {
    payload: editPayload(workspace),
    env: fakeEnv({ CARRICK_FAKE_FIXTURE: fixturePath("check-not-indexed.json") }),
  });
  assert.equal(notIndexed.stdout, "");
});

test("an edit to a file the index has no rows for runs no CLI at all", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const argvLog = path.join(workspace.root, "argv.log");

  const run = await runHook("post-edit.ts", {
    payload: {
      cwd: workspace.root,
      tool_input: { file_path: path.join(workspace.root, "README.md") },
    },
    env: fakeEnv({ CARRICK_FAKE_ARGV_LOG: argvLog }),
  });
  assert.equal(run.stdout, "");
  assert.equal(fs.existsSync(argvLog), false);
});

test("an edit asks for the file to be re-judged from the working tree", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const argvLog = path.join(workspace.root, "argv.log");

  await runHook("post-edit.ts", {
    payload: editPayload(workspace),
    env: fakeEnv({ CARRICK_FAKE_ARGV_LOG: argvLog }),
  });
  const call = firstCall(argvLog);
  assert.ok(call);
  // The language server's own call is asserted without it in server.test.ts:
  // this surface fires once per completed edit, that one fires on every save
  // (carrick#1036).
  assert.ok(call.argv.includes("--recheck"), call.argv.join(" "));
});

test("a broken CLI never fails the edit", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  const failed = await runHook("post-edit.ts", {
    payload: editPayload(workspace),
    env: fakeEnv({ CARRICK_FAKE_EXIT: "3" }),
  });
  assert.equal(failed.code, 0);
  assert.equal(failed.stdout, "");

  const missing = await runHook("post-edit.ts", {
    payload: editPayload(workspace),
    env: fakeEnv({ CARRICK_BIN: "/nonexistent/carrick" }),
  });
  assert.equal(missing.code, 0);
  assert.equal(missing.stdout, "");

  const garbage = await runHook("post-edit.ts", { payload: "not a payload" });
  assert.equal(garbage.code, 0);
  assert.equal(garbage.stdout, "");
});

test("a slow CLI is abandoned, not waited on", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  const run = await runHook("post-edit.ts", {
    payload: editPayload(workspace),
    env: fakeEnv({ CARRICK_FAKE_DELAY_MS: "4000", CARRICK_TIMEOUT_MS: "250" }),
  });
  assert.equal(run.code, 0);
  assert.equal(run.stdout, "");
  assert.ok(run.ms < 3000, `hook returned in ${run.ms}ms rather than waiting out the CLI`);
});

test("the hook's own work fits the 300 ms budget", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  const run = await runHook("post-edit.ts", {
    payload: editPayload(workspace),
    env: fakeEnv({ CARRICK_LOG_QUIET: "0" }),
  });
  const cliMs = Number(/in (\d+)ms/.exec(run.stderr)?.[1] ?? "0");
  const ours = run.ms - cliMs;
  assert.ok(cliMs >= 0);
  assert.ok(ours < 300, `hook overhead was ${ours}ms (total ${run.ms}ms, CLI ${cliMs}ms)`);
});

test("the hook stays quiet when the LSP owns delivery", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  for (const channel of ["lsp", "off"]) {
    const run = await runHook("post-edit.ts", {
      payload: editPayload(workspace),
      env: fakeEnv({ CARRICK_CHANNEL: channel }),
    });
    assert.equal(run.stdout, "", `channel ${channel} printed something`);
  }
});

test("the session line asks status about the workspace, and states what it holds", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const argvLog = path.join(workspace.root, "argv.log");

  const run = await runHook("session-start.ts", {
    cwd: workspace.root,
    env: fakeEnv({
      CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json"),
      CARRICK_FAKE_ARGV_LOG: argvLog,
    }),
  });
  assert.equal(run.code, 0);
  assert.match(run.stdout, /^Carrick indexed 3 service\(s\) in \S+ at 2026-09-06T21:14:03Z/);
  // A line a developer runs by hand, so it ends like one (#838).
  assert.ok(run.stdout.endsWith("\n"), "the session line ends with a newline");
  assert.ok(!run.stdout.endsWith("\n\n"), "and with exactly one");
  assert.match(run.stdout, /\n- user-service at 6a1b2c3: 157 route\(s\), 12 call\(s\), changed since index: 7 \(/);
  // Each service states the count of what its own scan reads (carrick#997).
  assert.match(run.stdout, /\n- user-admin at 6a1b2c3: 12 route\(s\), 3 call\(s\), changed since index: 7 \(/);
  const call = firstCall(argvLog);
  assert.ok(call);
  assert.deepEqual(call.argv.slice(0, 2), ["status", "--workspace"]);
  assert.equal(call.argv.at(-1), "--json");
  assert.equal(fs.realpathSync(call.cwd), fs.realpathSync(workspace.root));
});

test("no index gives the session one line and no session fails on it", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  const run = await runHook("session-start.ts", {
    cwd: workspace.root,
    env: fakeEnv({ CARRICK_FAKE_FIXTURE: fixturePath("status-not-indexed.json") }),
  });
  assert.equal(run.code, 0);
  assert.match(run.stdout, /^Carrick has no index for this workspace/);

  const broken = await runHook("session-start.ts", {
    cwd: workspace.root,
    env: fakeEnv({ CARRICK_FAKE_EXIT: "2" }),
  });
  assert.equal(broken.code, 0);
  assert.equal(broken.stdout, "");
});

test("the session line is printed whichever channel delivers verdicts", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  const lsp = await runHook("session-start.ts", {
    cwd: workspace.root,
    env: fakeEnv({
      CARRICK_CHANNEL: "lsp",
      CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json"),
    }),
  });
  assert.match(lsp.stdout, /^Carrick indexed/);

  const off = await runHook("session-start.ts", {
    cwd: workspace.root,
    env: fakeEnv({
      CARRICK_CHANNEL: "off",
      CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json"),
    }),
  });
  assert.equal(off.stdout, "");
});

// ---------------------------------------------------------------------------
// The reuse nudge (carrick#1330): the post-edit hook records, the Stop hook
// speaks once.

/** A home directory the hooks write their session records under. */
function fakeHome(t: { after: (fn: () => void) => void }): string {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-home-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function sessionRecord(home: string, session: string): { found: unknown[]; nudged: string[] } {
  return JSON.parse(
    fs.readFileSync(path.join(home, ".carrick", "sessions", `${session}.json`), "utf8"),
  ) as { found: unknown[]; nudged: string[] };
}

test("an edit that adds a function records it and says nothing about it", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);

  const withNew = await runHook("post-edit.ts", {
    payload: { ...editPayload(workspace), session_id: "sess-1" },
    env: fakeEnv({ HOME: home, CARRICK_FAKE_FIXTURE: fixturePath("check-new-functions.json") }),
  });
  assert.equal(withNew.code, 0);

  // The names are on disk, with the file and the commit they were compared at.
  const record = sessionRecord(home, "sess-1");
  assert.deepEqual(
    record.found.map((entry) => (entry as { name: string }).name),
    ["slugify", "titleCase"],
  );
  assert.equal(record.nudged.length, 0);

  // And nothing about them was printed. The comparison is against the same
  // payload with the block removed, so the assertion cannot pass by the hook
  // simply having been quiet for another reason.
  const quiet = fixturePath("check-new-functions.json");
  const without = path.join(home, "no-new-functions.json");
  const body = JSON.parse(fs.readFileSync(quiet, "utf8")) as { recheck: { new_functions?: unknown } };
  delete body.recheck.new_functions;
  fs.writeFileSync(without, JSON.stringify(body));
  const withoutNew = await runHook("post-edit.ts", {
    payload: { ...editPayload(workspace), session_id: "sess-2" },
    env: fakeEnv({ HOME: home, CARRICK_FAKE_FIXTURE: without }),
  });
  assert.equal(withNew.stdout, withoutNew.stdout, "the new functions changed no printed byte");
  assert.equal(fs.existsSync(path.join(home, ".carrick", "sessions", "sess-2.json")), false);
});

test("the stop hook names a task's new functions to the model, once", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);

  await runHook("post-edit.ts", {
    payload: { ...editPayload(workspace), session_id: "sess-3" },
    env: fakeEnv({ HOME: home, CARRICK_FAKE_FIXTURE: fixturePath("check-new-functions.json") }),
  });

  const stop = await runHook("stop.ts", {
    payload: { hook_event_name: "Stop", session_id: "sess-3", stop_hook_active: false },
    env: fakeEnv({ HOME: home }),
  });
  assert.equal(stop.code, 0);
  const parsed = JSON.parse(stop.stdout) as {
    hookSpecificOutput: { hookEventName: string; additionalContext: string };
    decision?: unknown;
    continue?: unknown;
    systemMessage?: unknown;
  };
  // The one Stop channel Claude Code delivers to the model while letting the
  // turn continue. `systemMessage` never reaches the model and `decision:
  // "block"` reaches it by refusing to stop, which is a block.
  assert.equal(parsed.hookSpecificOutput.hookEventName, "Stop");
  assert.equal(parsed.decision, undefined);
  assert.equal(parsed.continue, undefined);
  assert.equal(parsed.systemMessage, undefined);
  assert.match(parsed.hookSpecificOutput.additionalContext, /slugify/);
  assert.match(parsed.hookSpecificOutput.additionalContext, /carrick-reuse/);

  // A task is several stops. The second one says nothing about the same set.
  const again = await runHook("stop.ts", {
    payload: { hook_event_name: "Stop", session_id: "sess-3" },
    env: fakeEnv({ HOME: home }),
  });
  assert.equal(again.stdout, "");
  assert.equal(again.code, 0);
});

test("the stop hook is silent with nothing recorded, no session, or the channel off", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);

  for (const payload of [
    { hook_event_name: "Stop", session_id: "never-edited" },
    { hook_event_name: "Stop" },
    {},
  ]) {
    const run = await runHook("stop.ts", { payload, env: fakeEnv({ HOME: home }) });
    assert.equal(run.stdout, "", `expected silence for ${JSON.stringify(payload)}`);
    assert.equal(run.code, 0);
  }

  await runHook("post-edit.ts", {
    payload: { ...editPayload(workspace), session_id: "sess-4" },
    env: fakeEnv({ HOME: home, CARRICK_FAKE_FIXTURE: fixturePath("check-new-functions.json") }),
  });
  const off = await runHook("stop.ts", {
    payload: { hook_event_name: "Stop", session_id: "sess-4" },
    env: fakeEnv({ HOME: home, CARRICK_CHANNEL: "off" }),
  });
  assert.equal(off.stdout, "");
});

test("an install where the language server delivers still records for the stop hook", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);

  // `lsp` decides who states a verdict about an edited file. It says nothing
  // about the end of a task, which the language server has no notion of.
  const run = await runHook("post-edit.ts", {
    payload: { ...editPayload(workspace), session_id: "sess-5" },
    env: fakeEnv({
      HOME: home,
      CARRICK_CHANNEL: "lsp",
      CARRICK_FAKE_FIXTURE: fixturePath("check-new-functions.json"),
    }),
  });
  assert.equal(run.stdout, "", "the lsp channel owns the printing");
  assert.equal(sessionRecord(home, "sess-5").found.length, 2);
});
