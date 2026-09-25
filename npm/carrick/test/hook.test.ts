// The PostToolUse, SessionStart and Stop hooks, run as Claude Code runs them.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import {
  editPayload,
  fakeEnv,
  firstCall,
  fixturePath,
  makeWorkspace,
  runHook,
  testDir,
} from "./helpers.ts";

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

  // CPU time on the hook's main thread, which excludes the CLI it spawns. Wall
  // time on a shared runner measured the concurrent test files too
  // (carrick#1498); cpu-usage.mjs gives the margin.
  const run = await runHook("post-edit.ts", {
    payload: editPayload(workspace),
    env: fakeEnv({ CARRICK_LOG_QUIET: "0" }),
    nodeArgs: [`--import=${path.join(testDir, "cpu-usage.mjs")}`],
  });
  assert.equal(run.code, 0);
  assert.match(run.stdout, /additionalContext/);
  const cpu = /carrick-test-cpu-ms=(\d+)/.exec(run.stderr);
  assert.ok(cpu, `the hook reported no CPU time: ${run.stderr}`);
  const ours = Number(cpu[1]);
  const cli = /in (\d+)ms/.exec(run.stderr);
  assert.ok(cli, `the hook logged no CLI time: ${run.stderr}`);
  const waited = run.ms - Number(cli[1]);
  t.diagnostic(`hook CPU ${ours}ms, wall ${run.ms}ms, wall less CLI ${waited}ms`);
  assert.ok(ours < 300, `hook used ${ours}ms of CPU (wall ${run.ms}ms)`);
  // CPU time does not count waiting, so a hook that blocks on the network or
  // a timer passes the bound above. This catches it. 2 s is far above the
  // 322 ms a loaded shared runner has measured, so contention does not trip
  // it, and far below the seconds a network wait takes.
  assert.ok(waited < 2000, `hook spent ${waited}ms of wall time outside the CLI`);
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

// ---------------------------------------------------------------------------
// The same nudge on Codex (carrick#1335): `apply_patch` on the recording side,
// `UserPromptSubmit` on the delivery side.

/** Codex's PostToolUse payload: one patch, several files, paths from the cwd. */
function patchPayload(workspace: { root: string }, session: string) {
  return {
    hook_event_name: "PostToolUse",
    tool_name: "apply_patch",
    cwd: workspace.root,
    session_id: session,
    tool_input: {
      command: [
        "*** Begin Patch",
        "*** Add File: user-service/src/routes/users.ts",
        "+export function slugify(input: string): string {",
        '+  return input.toLowerCase();',
        "+}",
        "*** Update File: billing-service/src/lookup.ts",
        "@@",
        "-const a = 1;",
        "+const a = 2;",
        "*** Delete File: billing-service/src/charges.ts",
        "*** Add File: notes/README.md",
        "+notes",
        "*** End Patch",
      ].join("\n"),
    },
  };
}

test("a Codex patch is re-checked file by file, and the deleted one is not", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);
  const argvLog = path.join(home, "argv.log");

  const run = await runHook("post-edit.ts", {
    payload: patchPayload(workspace, "codex-1"),
    env: fakeEnv({
      HOME: home,
      CARRICK_FAKE_ARGV_LOG: argvLog,
      CARRICK_FAKE_FIXTURE: fixturePath("check-new-functions.json"),
    }),
  });
  assert.equal(run.code, 0);

  const asked = fs
    .readFileSync(argvLog, "utf8")
    .split("\n")
    .filter((line) => line.trim().length > 0)
    .map((line) => (JSON.parse(line) as { argv: string[] }).argv[1]);
  assert.deepEqual(asked, [
    "user-service/src/routes/users.ts",
    "billing-service/src/lookup.ts",
  ]);
  // The deleted file is gone from disk, so asking about it would be asking
  // about nothing; the markdown file has no rows in any index.
  assert.equal(asked.includes("billing-service/src/charges.ts"), false);
  assert.equal(asked.includes("notes/README.md"), false);

  // Recorded exactly as an Edit's new functions are, and still silent about it.
  assert.equal(sessionRecord(home, "codex-1").found.length, 2);
});

test("the Codex hook names the set on the next prompt, once", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);

  await runHook("post-edit.ts", {
    payload: patchPayload(workspace, "codex-2"),
    env: fakeEnv({ HOME: home, CARRICK_FAKE_FIXTURE: fixturePath("check-new-functions.json") }),
  });

  const prompt = await runHook("user-prompt.ts", {
    payload: {
      hook_event_name: "UserPromptSubmit",
      session_id: "codex-2",
      cwd: workspace.root,
      prompt: "now write the exporter",
    },
    env: fakeEnv({ HOME: home }),
  });
  assert.equal(prompt.code, 0);
  const parsed = JSON.parse(prompt.stdout) as {
    hookSpecificOutput: { hookEventName: string; additionalContext: string };
    decision?: unknown;
    continue?: unknown;
    systemMessage?: unknown;
  };
  // Codex parses this with `deny_unknown_fields` and accepts
  // `additionalContext` on this event and not on `Stop`, so the event name is
  // the load-bearing byte. A `decision` here would be the block the ruling
  // refused.
  assert.equal(parsed.hookSpecificOutput.hookEventName, "UserPromptSubmit");
  assert.equal(parsed.decision, undefined);
  assert.equal(parsed.continue, undefined);
  assert.equal(parsed.systemMessage, undefined);
  assert.match(parsed.hookSpecificOutput.additionalContext, /slugify/);
  assert.match(parsed.hookSpecificOutput.additionalContext, /carrick-reuse/);

  // The prompt after it says nothing: the set was marked when it was spoken.
  const again = await runHook("user-prompt.ts", {
    payload: { hook_event_name: "UserPromptSubmit", session_id: "codex-2" },
    env: fakeEnv({ HOME: home }),
  });
  assert.equal(again.stdout, "");
  assert.equal(again.code, 0);
});

test("one session is nudged once across both hosts, whichever speaks first", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);

  await runHook("post-edit.ts", {
    payload: { ...editPayload(workspace), session_id: "codex-3" },
    env: fakeEnv({ HOME: home, CARRICK_FAKE_FIXTURE: fixturePath("check-new-functions.json") }),
  });
  const stop = await runHook("stop.ts", {
    payload: { hook_event_name: "Stop", session_id: "codex-3" },
    env: fakeEnv({ HOME: home }),
  });
  assert.match(stop.stdout, /slugify/);

  // Both hosts drain the same store through the same function, so a set one has
  // spoken is not waiting for the other.
  const prompt = await runHook("user-prompt.ts", {
    payload: { hook_event_name: "UserPromptSubmit", session_id: "codex-3" },
    env: fakeEnv({ HOME: home }),
  });
  assert.equal(prompt.stdout, "");
});

test("the Codex hook is silent with nothing recorded, no session, or the channel off", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);

  for (const payload of [
    { hook_event_name: "UserPromptSubmit", session_id: "never-edited" },
    { hook_event_name: "UserPromptSubmit" },
    {},
  ]) {
    const run = await runHook("user-prompt.ts", { payload, env: fakeEnv({ HOME: home }) });
    assert.equal(run.stdout, "", `expected silence for ${JSON.stringify(payload)}`);
    assert.equal(run.code, 0);
  }

  await runHook("post-edit.ts", {
    payload: patchPayload(workspace, "codex-4"),
    env: fakeEnv({ HOME: home, CARRICK_FAKE_FIXTURE: fixturePath("check-new-functions.json") }),
  });
  const off = await runHook("user-prompt.ts", {
    payload: { hook_event_name: "UserPromptSubmit", session_id: "codex-4" },
    env: fakeEnv({ HOME: home, CARRICK_CHANNEL: "off" }),
  });
  assert.equal(off.stdout, "");
});
