// The PostToolUse and SessionStart hooks, run as Claude Code runs them.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { editPayload, fakeEnv, fixturePath, makeWorkspace, runHook } from "./helpers.ts";

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
  assert.match(context, /41 candidate\(s\) not classified locally/);
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

test("the session line states the index and the drift", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const argvLog = path.join(workspace.root, "argv.log");

  const run = await runHook("session-start.ts", {
    cwd: workspace.root,
    env: fakeEnv({
      CARRICK_FAKE_FIXTURE: fixturePath("touch-workspace.json"),
      CARRICK_FAKE_ARGV_LOG: argvLog,
    }),
  });
  assert.equal(run.code, 0);
  assert.match(run.stdout, /^Carrick indexed user-service at 6a1b2c3\. 7 file\(s\) have changed/);
  const call = JSON.parse(fs.readFileSync(argvLog, "utf8").trim()) as { argv: string[] };
  assert.deepEqual(call.argv, ["touch", "--json"]);
});

test("the session line is printed whichever channel delivers verdicts", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  const lsp = await runHook("session-start.ts", {
    cwd: workspace.root,
    env: fakeEnv({
      CARRICK_CHANNEL: "lsp",
      CARRICK_FAKE_FIXTURE: fixturePath("touch-workspace.json"),
    }),
  });
  assert.match(lsp.stdout, /^Carrick indexed/);

  const off = await runHook("session-start.ts", {
    cwd: workspace.root,
    env: fakeEnv({
      CARRICK_CHANNEL: "off",
      CARRICK_FAKE_FIXTURE: fixturePath("touch-workspace.json"),
    }),
  });
  assert.equal(off.stdout, "");
});
