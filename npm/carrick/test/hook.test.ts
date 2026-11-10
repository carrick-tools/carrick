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
import { invokerValue } from "../src/hook/invoker.ts";
import { readSession } from "../src/hook/reuse.ts";

test("an edit gets its problems as additionalContext", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  const run = await runHook("post-edit.ts", {
    payload: editPayload(workspace),
    env: fakeEnv({ CARRICK_FAKE_FIXTURE: fixturePath("check-counterpart.json") }),
  });
  assert.equal(run.code, 0);
  const parsed = JSON.parse(run.stdout) as {
    hookSpecificOutput: { hookEventName: string; additionalContext: string };
  };
  assert.equal(parsed.hookSpecificOutput.hookEventName, "PostToolUse");
  assert.match(
    parsed.hookSpecificOutput.additionalContext,
    /^Carrick re-checked user-service\/src\/routes\/users\.ts after this edit: 1 contract problem\.\n- /,
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
    env: fakeEnv({ CARRICK_FAKE_FIXTURE: fixturePath("check-counterpart.json") }),
  });
  assert.equal(run.code, 0);
  assert.match(run.stdout, /additionalContext/);
});

test("an edit that matches the index, or to a file with no indexed rows, prints nothing", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  for (const name of ["check-clean.json", "check-silent.json"]) {
    const run = await runHook("post-edit.ts", {
      payload: editPayload(workspace),
      env: fakeEnv({ CARRICK_FAKE_FIXTURE: fixturePath(name) }),
    });
    assert.equal(run.code, 0, name);
    assert.equal(run.stdout, "", name);
  }
  // A file the index holds, restored to its indexed text, is silent too.
  const same = path.join(workspace.root, "same.json");
  const body = JSON.parse(fs.readFileSync(fixturePath("check-mismatch.json"), "utf8")) as Record<string, unknown>;
  body["items"] = [];
  body["stale"] = false;
  body["recheck"] = { ran: "none", elapsed_ms: 0, reason: "the file has not changed since the index" };
  fs.writeFileSync(same, JSON.stringify(body));
  const run = await runHook("post-edit.ts", {
    payload: editPayload(workspace),
    env: fakeEnv({ CARRICK_FAKE_FIXTURE: same }),
  });
  assert.equal(run.stdout, "");
});

test("the hook is silent when the index cannot answer", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

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
    env: fakeEnv({ CARRICK_LOG_QUIET: "0", CARRICK_FAKE_FIXTURE: fixturePath("check-counterpart.json") }),
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
  assert.match(run.stdout, /\n- user-service at 6a1b2c3: 157 route\(s\), 12 call\(s\), changed since index: 7\n/);
  // Each service states the count of what its own scan reads (carrick#997).
  assert.match(run.stdout, /\n- user-admin at 6a1b2c3: 12 route\(s\), 3 call\(s\), changed since index: 7\n/);
  const call = firstCall(argvLog);
  assert.ok(call);
  assert.deepEqual(call.argv.slice(0, 2), ["status", "--workspace"]);
  assert.equal(call.argv.at(-1), "--json");
  assert.equal(fs.realpathSync(call.cwd), fs.realpathSync(workspace.root));
});

// What Codex's own parser takes from a SessionStart hook
// (`codex-rs/hooks/src/engine/output_parser.rs`, `parse_session_start`): one JSON
// object whose `hookSpecificOutput.additionalContext` is the context. It reads
// nothing else off stdout as context.
test("under Codex the session line goes out as additionalContext; Claude Code keeps the plain print", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const env = (extra: Record<string, string>) =>
    fakeEnv({ CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json"), ...extra });

  const plain = await runHook("session-start.ts", {
    cwd: workspace.root,
    payload: { hook_event_name: "SessionStart" },
    env: env({ CLAUDE_PROJECT_DIR: workspace.root }),
  });
  assert.match(plain.stdout, /^Carrick indexed 3 service\(s\)/);

  const codex = await runHook("session-start.ts", {
    cwd: workspace.root,
    payload: { hook_event_name: "SessionStart", session_id: "s", cwd: workspace.root },
    env: env({ CLAUDE_PROJECT_DIR: "" }),
  });
  assert.equal(codex.code, 0);
  const wire = JSON.parse(codex.stdout) as Record<string, unknown>;
  assert.deepEqual(Object.keys(wire), ["hookSpecificOutput"]);
  const output = wire["hookSpecificOutput"] as { hookEventName: string; additionalContext: string };
  assert.equal(output.hookEventName, "SessionStart");
  // Both carry the finish-line check.
  assert.match(plain.stdout, /\nWhen your change touches a route, a call to another service, an event or a type another service reads, run `carrick check --changed` before you call it done\. Exit 1 is a contract problem; exit 2 means nothing could be checked\.\n$/);
  // The same text Claude Code is given, byte for byte.
  assert.equal(`${output.additionalContext}\n`, plain.stdout);
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
// No reuse nudge at the end of a task (carrick#3436): an edit that adds a
// function records nothing about it, and neither host's end-of-task hook
// names it. The Stop hook's re-check wait is `stop-wait.test.ts`.

/** A home directory the hooks write their session records under. */
function fakeHome(t: { after: (fn: () => void) => void }): string {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-home-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function sessionRecord(home: string, session: string): Record<string, unknown> {
  return JSON.parse(
    fs.readFileSync(path.join(home, ".carrick", "sessions", `${session}.json`), "utf8"),
  ) as Record<string, unknown>;
}

test("an edit that adds a function prints and records nothing about it (carrick#3436)", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);

  const withNew = await runHook("post-edit.ts", {
    payload: { ...editPayload(workspace), session_id: "sess-1" },
    env: fakeEnv({ HOME: home, CARRICK_FAKE_FIXTURE: fixturePath("check-new-functions.json") }),
  });
  assert.equal(withNew.code, 0);

  // The session file holds the edit stamp and no list of functions.
  const record = sessionRecord(home, "sess-1");
  assert.equal("found" in record, false, JSON.stringify(record));
  assert.equal("nudged" in record, false, JSON.stringify(record));

  // Nothing about them was printed either. The comparison is against the same
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
});

test("the stop hook says nothing about the functions a task added (carrick#3436)", async (t) => {
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
  // The edit was answered in hand and holds no route or call: Stop says nothing.
  assert.equal(stop.stdout, "", "no reuse nudge, no summary");
  const again = await runHook("stop.ts", {
    payload: { hook_event_name: "Stop", session_id: "sess-3", stop_hook_active: true },
    env: fakeEnv({ HOME: home }),
  });
  assert.equal(again.stdout, "", "stated once per change");
});

test("an edit the binary refused (no index) gives Stop nothing to state and queues no check (carrick#3466)", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);
  await runHook("post-edit.ts", {
    payload: { ...editPayload(workspace), session_id: "sess-refused" },
    env: fakeEnv({ HOME: home, CARRICK_FAKE_FIXTURE: fixturePath("check-not-indexed.json") }),
  });
  const stop = await runHook("stop.ts", {
    payload: { hook_event_name: "Stop", session_id: "sess-refused" },
    env: fakeEnv({ HOME: home }),
  });
  assert.equal(stop.stdout, "");
  assert.equal(fs.existsSync(path.join(workspace.root, ".carrick", "rechecks", "queue")), false, "no check --changed queued");
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

// ---------------------------------------------------------------------------
// Codex (carrick#1335): `apply_patch` re-checks the patched files, and
// `UserPromptSubmit` delivers the finished re-checks Codex has no Stop for.

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

test("a Codex patch is re-checked in one call, and the deleted file is not asked about", async (t) => {
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

  const calls = fs
    .readFileSync(argvLog, "utf8")
    .split("\n")
    .filter((line) => line.trim().length > 0)
    .map((line) => (JSON.parse(line) as { argv: string[] }).argv);
  assert.equal(calls.length, 1, JSON.stringify(calls));
  assert.deepEqual(calls[0], [
    "check",
    "--each",
    "user-service/src/routes/users.ts",
    "billing-service/src/lookup.ts",
    "--json",
    "--recheck",
    "--no-rescan",
  ]);
  const asked = calls[0]!;
  // The deleted file is gone from disk, so asking about it would be asking
  // about nothing; the markdown file has no rows in any index.
  assert.equal(asked.includes("billing-service/src/charges.ts"), false);
  assert.equal(asked.includes("notes/README.md"), false);

  // The patch's new functions are not recorded, as an Edit's are not (carrick#3436).
  assert.equal("found" in sessionRecord(home, "codex-1"), false);
});

test("a three-file patch is one binary call, and every file gets its answer", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);
  const argvLog = path.join(home, "argv.log");
  const files = [
    "order-service/src/clients/users.ts",
    "order-service/src/server.ts",
    "billing-service/src/lookup.ts",
  ];
  const command = [
    "*** Begin Patch",
    ...files.flatMap((file) => [`*** Update File: ${file}`, "@@", "-const a = 1;", "+const a = 2;"]),
    "*** End Patch",
  ].join("\n");

  const run = await runHook("post-edit.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "apply_patch",
      cwd: workspace.root,
      session_id: "codex-3",
      tool_input: { command },
    },
    env: fakeEnv({ HOME: home, CARRICK_FAKE_ARGV_LOG: argvLog }),
  });
  assert.equal(run.code, 0);
  const calls = fs
    .readFileSync(argvLog, "utf8")
    .split("\n")
    .filter((line) => line.trim().length > 0);
  assert.equal(calls.length, 1, calls.join("\n"));
  // None of the three was re-checked: a gap is not a finding, so nothing prints.
  assert.equal(run.stdout, "");
});

test("a repo whose re-check is known not to fit prints nothing on every edit", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);
  const knownMiss = path.join(home, "check-known-miss.json");
  const body = JSON.parse(fs.readFileSync(fixturePath("check-new-functions.json"), "utf8")) as {
    recheck: Record<string, unknown>;
  };
  body.recheck = {
    ran: "none",
    elapsed_ms: 40,
    stale_since: "2026-09-06T21:14:03Z",
    reason: "an earlier re-check of this repo ran past its budget against this index",
    known_miss: true,
    new_functions: [{ name: "slugify", line: 12 }],
  };
  fs.writeFileSync(knownMiss, JSON.stringify(body));
  const env = fakeEnv({ HOME: home, CARRICK_FAKE_FIXTURE: knownMiss });
  const edit = (session: string) =>
    runHook("post-edit.ts", {
      payload: { ...editPayload(workspace), session_id: session },
      env,
    });

  const first = contextOf((await edit("sess-km")).stdout);
  const second = contextOf((await edit("sess-km")).stdout);
  // The index's verdicts describe the file before the edit, so none is
  // printed, and a gap is not a finding: the Stop hook names it once.
  assert.equal(first, "");
  assert.equal(second, "");
});

test("the Codex hook says nothing on the next prompt about the functions a patch added (carrick#3436)", async (t) => {
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
  assert.equal(prompt.stdout, "", "no reuse nudge, and no re-check was pending");
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

// ---------------------------------------------------------------------------
// Who uses the edited file (carrick#2067): shown once per file per session.

function contextOf(stdout: string): string {
  if (!stdout) return "";
  return (JSON.parse(stdout) as { hookSpecificOutput: { additionalContext: string } })
    .hookSpecificOutput.additionalContext;
}

const USES_LINE =
  "- CatalogClient.readWidget (line 12) has 2 caller(s) outside this file: src/inventory.ts:9, src/inventory.ts:17";
/** The one line an edit gets about who uses the file; the Read hook lists them. */
const EDIT_USES = "Other files use what this file declares";

test("a file's uses line is shown once per session, and a clean edit prints nothing else", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);
  const env = fakeEnv({ HOME: home, CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json") });
  const edit = (session?: string) =>
    runHook("post-edit.ts", {
      payload: session ? { ...editPayload(workspace), session_id: session } : editPayload(workspace),
      env,
    });

  const first = contextOf((await edit("sess-u1")).stdout);
  const second = contextOf((await edit("sess-u1")).stdout);
  assert.ok(first.includes(EDIT_USES), first);
  // The edit counts the uses; listing them is the Read hook's job.
  assert.equal(first.includes(USES_LINE), false, first);
  assert.ok(first.startsWith(EDIT_USES) && !first.includes("\n"), first);
  assert.equal(second, "");

  // Another session has not been shown them.
  assert.ok(contextOf((await edit("sess-u2")).stdout).includes(EDIT_USES));
  // With no session to remember them in, every edit shows them.
  assert.ok(contextOf((await edit()).stdout).includes(EDIT_USES));
  assert.ok(contextOf((await edit()).stdout).includes(EDIT_USES));
});

test("a channel that prints nothing remembers nothing as shown", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);
  const payload = { ...editPayload(workspace), session_id: "sess-u3" };
  const fixture = fixturePath("check-uses.json");

  const lsp = await runHook("post-edit.ts", {
    payload,
    env: fakeEnv({ HOME: home, CARRICK_CHANNEL: "lsp", CARRICK_FAKE_FIXTURE: fixture }),
  });
  assert.equal(lsp.stdout, "");
  const record = path.join(home, ".carrick", "sessions", "sess-u3.json");
  const told = fs.existsSync(record)
    ? ((JSON.parse(fs.readFileSync(record, "utf8")) as { told?: string[] }).told ?? [])
    : [];
  assert.deepEqual(told, []);

  const hook = await runHook("post-edit.ts", {
    payload,
    env: fakeEnv({ HOME: home, CARRICK_FAKE_FIXTURE: fixture }),
  });
  assert.ok(contextOf(hook.stdout).includes(EDIT_USES));
});

// ---------------------------------------------------------------------------
// The Read hook (carrick#2069): who depends on a file, when the agent opens it.

function readPayload(workspace: { root: string; file: string }, session?: string) {
  return {
    hook_event_name: "PostToolUse",
    tool_name: "Read",
    cwd: workspace.root,
    tool_input: { file_path: workspace.file },
    tool_response: { file: { filePath: workspace.file, content: "const a = 1;" } },
    ...(session ? { session_id: session } : {}),
  };
}

function runCount(argvLog: string): number {
  return fs.existsSync(argvLog)
    ? fs.readFileSync(argvLog, "utf8").split("\n").filter((line) => line.trim()).length
    : 0;
}

test("the first Read of a file prints who uses it, with a plain check", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);
  const argvLog = path.join(workspace.root, "argv.log");

  const run = await runHook("post-read.ts", {
    payload: readPayload(workspace, "sess-r1"),
    env: fakeEnv({
      HOME: home,
      CARRICK_FAKE_ARGV_LOG: argvLog,
      CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json"),
    }),
  });
  assert.equal(run.code, 0);
  const context = contextOf(run.stdout);
  assert.ok(context.includes(USES_LINE), context);
  assert.match(context, /^Carrick: other code depends on user-service\/src\/routes\/users\.ts/);
  const call = firstCall(argvLog);
  assert.ok(call);
  assert.deepEqual(call.argv.slice(0, 1), ["check"]);
  assert.ok(call.argv.includes("--json"));
  assert.equal(call.argv.includes("--recheck"), false, call.argv.join(" "));
});

test("a second Read of the same file in a session prints nothing and starts no process", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);
  const argvLog = path.join(workspace.root, "argv.log");
  const env = fakeEnv({
    HOME: home,
    CARRICK_FAKE_ARGV_LOG: argvLog,
    CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json"),
  });

  const first = await runHook("post-read.ts", { payload: readPayload(workspace, "sess-r2"), env });
  assert.ok(contextOf(first.stdout).includes(USES_LINE));
  const second = await runHook("post-read.ts", { payload: readPayload(workspace, "sess-r2"), env });
  assert.equal(second.stdout, "");
  assert.equal(runCount(argvLog), 1);
  // Another session is asked afresh.
  const other = await runHook("post-read.ts", { payload: readPayload(workspace, "sess-r2b"), env });
  assert.ok(contextOf(other.stdout).includes(USES_LINE));
  assert.equal(runCount(argvLog), 2);
});

test("a Read prints the other side of a route or call, and no verdict words", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);

  const run = await runHook("post-read.ts", {
    payload: readPayload(workspace, "sess-r3"),
    env: fakeEnv({ HOME: home, CARRICK_FAKE_FIXTURE: fixturePath("check-counterpart.json") }),
  });
  const context = contextOf(run.stdout);
  assert.match(
    context,
    /- user-service\/src\/routes\/users\.ts:18:3 call GET \/api\/users\/:id\. Producers: user-service src\/routes\/users\.ts:42/,
  );
  assert.doesNotMatch(context, /type_mismatch|no longer carries|\[fact|Read off/);
});

test("a Read of a file nothing depends on prints nothing, not even a header", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);

  for (const name of ["check-clean.json", "check-silent.json"]) {
    const run = await runHook("post-read.ts", {
      payload: readPayload(workspace, `sess-r4-${name}`),
      env: fakeEnv({ HOME: home, CARRICK_FAKE_FIXTURE: fixturePath(name) }),
    });
    assert.equal(run.code, 0, name);
    assert.equal(run.stdout, "", name);
  }
});

test("a Read of a file that is not TypeScript starts no process", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const argvLog = path.join(workspace.root, "argv.log");

  const run = await runHook("post-read.ts", {
    payload: {
      tool_name: "Read",
      cwd: workspace.root,
      session_id: "sess-r5",
      tool_input: { file_path: path.join(workspace.root, "README.md") },
    },
    env: fakeEnv({ CARRICK_FAKE_ARGV_LOG: argvLog }),
  });
  assert.equal(run.code, 0);
  assert.equal(run.stdout, "");
  assert.equal(runCount(argvLog), 0);
});

test("a Read the index cannot answer, or that runs past its limit, prints nothing and exits 0", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);
  const argvLog = path.join(workspace.root, "argv.log");

  const notIndexed = await runHook("post-read.ts", {
    payload: readPayload(workspace, "sess-r6a"),
    env: fakeEnv({ HOME: home, CARRICK_FAKE_FIXTURE: fixturePath("check-not-indexed.json") }),
  });
  assert.equal(notIndexed.code, 0);
  assert.equal(notIndexed.stdout, "");

  const slow = await runHook("post-read.ts", {
    payload: readPayload(workspace, "sess-r6b"),
    env: fakeEnv({
      HOME: home,
      CARRICK_FAKE_ARGV_LOG: argvLog,
      CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json"),
      CARRICK_FAKE_DELAY_MS: "4000",
      CARRICK_TIMEOUT_MS: "250",
    }),
  });
  assert.equal(slow.code, 0);
  assert.equal(slow.stdout, "");
  // Asked once: the timeout is not asked again on the next Read.
  await runHook("post-read.ts", {
    payload: readPayload(workspace, "sess-r6b"),
    env: fakeEnv({ HOME: home, CARRICK_FAKE_ARGV_LOG: argvLog }),
  });
  assert.equal(runCount(argvLog), 1);
});

test("a Read and then an Edit: the clean edit prints neither a header nor the uses lines", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);
  const env = fakeEnv({ HOME: home, CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json") });

  const read = await runHook("post-read.ts", { payload: readPayload(workspace, "sess-r7"), env });
  assert.ok(contextOf(read.stdout).includes(USES_LINE));
  const edit = await runHook("post-edit.ts", {
    payload: { ...editPayload(workspace), session_id: "sess-r7" },
    env,
  });
  const context = contextOf(edit.stdout);
  assert.equal(context, "");
});

test("the lsp and off channels print nothing from a Read and write no session file", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);
  const argvLog = path.join(workspace.root, "argv.log");

  for (const channel of ["lsp", "off"]) {
    const run = await runHook("post-read.ts", {
      payload: readPayload(workspace, `sess-r8-${channel}`),
      env: fakeEnv({
        HOME: home,
        CARRICK_CHANNEL: channel,
        CARRICK_FAKE_ARGV_LOG: argvLog,
        CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json"),
      }),
    });
    assert.equal(run.code, 0, channel);
    assert.equal(run.stdout, "", channel);
  }
  assert.equal(runCount(argvLog), 0);
  assert.equal(fs.existsSync(path.join(home, ".carrick", "sessions")), false);
});

test("a hook marks the binary it spawns with who started it", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const invokerLog = path.join(workspace.root, "invoker.log");
  const calledAs = () => fs.readFileSync(invokerLog, "utf8").trim().split("\n");

  // A value already in the environment is the hook's to replace: the hook is
  // what ran the binary.
  await runHook("post-edit.ts", {
    payload: editPayload(workspace),
    env: fakeEnv({ CARRICK_FAKE_INVOKER_LOG: invokerLog, CARRICK_INVOKER: "cli" }),
  });
  assert.deepEqual(calledAs(), ["hook:post-edit"]);

  fs.rmSync(invokerLog);
  await runHook("session-start.ts", {
    cwd: workspace.root,
    env: fakeEnv({
      CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json"),
      CARRICK_FAKE_INVOKER_LOG: invokerLog,
    }),
  });
  assert.deepEqual(calledAs(), ["hook:session-start"]);
});

test("invokerValue names every hook the cloud log tells apart", () => {
  assert.deepEqual(
    (["post-edit", "stop", "session-start", "post-read"] as const).map(invokerValue),
    ["hook:post-edit", "hook:stop", "hook:session-start", "hook:post-read"],
  );
});

// ---------------------------------------------------------------------------
// The background re-check (B3): the hook asks a probe, queues what needs a
// re-scan, and the answer is delivered by whichever hook speaks next.

const LATE_USERS =
  "Carrick re-checked user-service/src/routes/users.ts after your earlier edit to it: 1 contract problem.";

/** Env for a workspace whose probe defers and whose full re-check finds one problem. */
function deferredEnv(home: string, extra: Record<string, string> = {}): NodeJS.ProcessEnv {
  return fakeEnv({
    HOME: home,
    CARRICK_FAKE_PROBE_FIXTURE: fixturePath("check-deferred.json"),
    CARRICK_FAKE_FIXTURE: fixturePath("check-counterpart.json"),
    ...extra,
  });
}

function answerFiles(home: string, session: string): string[] {
  const dir = path.join(home, ".carrick", "sessions");
  try {
    return fs.readdirSync(dir).filter((name) => name.startsWith(`${session}.recheck.`) && name.endsWith(".answer.json"));
  } catch {
    return [];
  }
}

/** Wait until the runner has written `count` answers for the session (so no runner outlives its test). */
async function waitForAnswers(home: string, session: string, count: number): Promise<void> {
  const until = Date.now() + 20_000;
  while (Date.now() < until) {
    if (answerFiles(home, session).length >= count) return;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  assert.fail(`the runner wrote fewer than ${count} answer(s) for ${session}`);
}

function secondFile(workspace: { root: string }): string {
  return path.join(workspace.root, "order-service", "src", "server.ts");
}

test("a deferred edit returns at once and prints nothing while the re-check runs behind it", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);

  const run = await runHook("post-edit.ts", {
    payload: { ...editPayload(workspace), session_id: "sess-b1" },
    env: deferredEnv(home, { CARRICK_FAKE_FULL_DELAY_MS: "5000" }),
  });
  assert.equal(run.code, 0);
  assert.ok(run.ms < 1500, `the hook waited ${run.ms}ms for a re-check that takes 5 s`);
  // Exactly one JSON object: parse the whole of stdout.
  assert.equal(run.stdout, "", "a queued edit says nothing until its answer holds a finding");
  await waitForAnswers(home, "sess-b1", 1);
});

test("the late answer arrives on the next post-edit, once, naming the earlier file", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);
  const env = deferredEnv(home, { CARRICK_FAKE_FULL_DELAY_MS: "200" });

  await runHook("post-edit.ts", { payload: { ...editPayload(workspace), session_id: "sess-b2" }, env });
  await waitForAnswers(home, "sess-b2", 1);

  const next = await runHook("post-edit.ts", {
    payload: {
      ...editPayload(workspace),
      session_id: "sess-b2",
      tool_input: { file_path: secondFile(workspace) },
    },
    env,
  });
  const lines = contextOf(next.stdout).split("\n\n");
  assert.ok(lines[0]?.startsWith(LATE_USERS), contextOf(next.stdout));
  assert.equal(lines.length, 1, "the pending line is gone; only the late answer speaks");
  await waitForAnswers(home, "sess-b2", 2);

  const third = await runHook("post-edit.ts", {
    payload: {
      ...editPayload(workspace),
      session_id: "sess-b2",
      tool_input: { file_path: path.join(workspace.root, "billing-service", "src", "lookup.ts") },
    },
    env,
  });
  assert.equal(contextOf(third.stdout).includes(LATE_USERS), false, "the first answer is printed once");
  await waitForAnswers(home, "sess-b2", 3);
});

test("a post-edit stamps the session's newest edit in its workspace, for the check clause", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);
  const env = deferredEnv(home, { CARRICK_FAKE_FULL_DELAY_MS: "100" });
  const before = Date.now();
  await runHook("post-edit.ts", { payload: { ...editPayload(workspace), session_id: "sess-stamp" }, env });
  await waitForAnswers(home, "sess-stamp", 1);
  const stamps = Object.values(readSession("sess-stamp", home).edited);
  assert.equal(stamps.length, 1);
  assert.ok(stamps[0]! >= before && stamps[0]! <= Date.now());
});

test("a Read delivers a finished answer, of a non-TypeScript file and of one already read", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);
  const env = deferredEnv(home, { CARRICK_FAKE_FULL_DELAY_MS: "100" });
  const read = (file: string) =>
    runHook("post-read.ts", {
      payload: { hook_event_name: "PostToolUse", tool_name: "Read", cwd: workspace.root, session_id: "sess-b3", tool_input: { file_path: file } },
      env,
    });

  await runHook("post-edit.ts", { payload: { ...editPayload(workspace), session_id: "sess-b3" }, env });
  await waitForAnswers(home, "sess-b3", 1);
  const md = await read(path.join(workspace.root, "README.md"));
  assert.ok(contextOf(md.stdout).startsWith(LATE_USERS), md.stdout);

  // A file read once is not asked about again, but a finished answer is still owed.
  const target = path.join(workspace.root, "order-service", "src", "clients", "users.ts");
  await read(target);
  await runHook("post-edit.ts", {
    payload: { ...editPayload(workspace), session_id: "sess-b3", tool_input: { file_path: secondFile(workspace) } },
    env,
  });
  await waitForAnswers(home, "sess-b3", 2);
  const again = await read(target);
  assert.ok(contextOf(again.stdout).startsWith("Carrick re-checked order-service/src/server.ts after your earlier edit to it"), again.stdout);
});

test("the Codex prompt hook delivers a finished answer", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);
  const env = deferredEnv(home, { CARRICK_FAKE_FULL_DELAY_MS: "100" });

  await runHook("post-edit.ts", { payload: { ...editPayload(workspace), session_id: "codex-b4" }, env });
  await waitForAnswers(home, "codex-b4", 1);
  const prompt = await runHook("user-prompt.ts", {
    payload: { hook_event_name: "UserPromptSubmit", session_id: "codex-b4", cwd: workspace.root },
    env,
  });
  const parsed = JSON.parse(prompt.stdout) as {
    hookSpecificOutput: { hookEventName: string; additionalContext: string };
    decision?: unknown;
    continue?: unknown;
    systemMessage?: unknown;
  };
  // Codex parses this with `deny_unknown_fields` and accepts
  // `additionalContext` on this event and not on `Stop`, so the event name is
  // the load-bearing byte. A `decision` here would be a block.
  assert.equal(parsed.hookSpecificOutput.hookEventName, "UserPromptSubmit");
  assert.equal(parsed.decision, undefined);
  assert.equal(parsed.continue, undefined);
  assert.equal(parsed.systemMessage, undefined);
  assert.ok(parsed.hookSpecificOutput.additionalContext.startsWith(LATE_USERS));
});

test("an answer in hand prints at once and starts no runner", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);
  const argvLog = path.join(workspace.root, "argv.log");

  const run = await runHook("post-edit.ts", {
    payload: { ...editPayload(workspace), session_id: "sess-b5" },
    env: fakeEnv({ HOME: home, CARRICK_FAKE_ARGV_LOG: argvLog, CARRICK_FAKE_FIXTURE: fixturePath("check-counterpart.json") }),
  });
  assert.match(contextOf(run.stdout), /^Carrick re-checked user-service\/src\/routes\/users\.ts after this edit: 1 contract problem\./);
  const calls = fs.readFileSync(argvLog, "utf8").trim().split("\n").map((l) => (JSON.parse(l) as { argv: string[] }).argv);
  assert.equal(calls.length, 1);
  assert.ok(calls.every((argv) => argv.includes("--no-rescan")), "every call is a probe");
  assert.equal(fs.existsSync(path.join(workspace.root, ".carrick", "rechecks")), false);
});

test("a GraphQL schema edit runs the probe", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const argvLog = path.join(workspace.root, "argv.log");
  const schema = path.join(workspace.service, "schema.graphql");
  fs.writeFileSync(schema, "type Query { me: String }\n");

  await runHook("post-edit.ts", {
    payload: { ...editPayload(workspace), tool_input: { file_path: schema } },
    env: fakeEnv({ CARRICK_FAKE_ARGV_LOG: argvLog }),
  });
  const call = firstCall(argvLog);
  assert.deepEqual(call?.argv, ["check", "user-service/schema.graphql", "--json", "--recheck", "--no-rescan"]);
});

test("a probe that fails still queues the edit", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);

  const run = await runHook("post-edit.ts", {
    payload: { ...editPayload(workspace), session_id: "sess-b8" },
    env: fakeEnv({ HOME: home, CARRICK_FAKE_EXIT: "3" }),
  });
  assert.equal(run.stdout, "");
  await waitForAnswers(home, "sess-b8", 1);
});

test("on the lsp channel nothing is queued and nothing is printed; with no session the probe answer prints", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const home = fakeHome(t);

  const lsp = await runHook("post-edit.ts", {
    payload: { ...editPayload(workspace), session_id: "sess-b9" },
    env: deferredEnv(home, { CARRICK_CHANNEL: "lsp" }),
  });
  assert.equal(lsp.stdout, "");
  assert.equal(fs.existsSync(path.join(workspace.root, ".carrick", "rechecks")), false);
  assert.equal("found" in sessionRecord(home, "sess-b9"), false);

  const none = await runHook("post-edit.ts", { payload: editPayload(workspace), env: deferredEnv(home) });
  assert.equal(none.stdout, "");
  assert.equal(fs.existsSync(path.join(workspace.root, ".carrick", "rechecks")), false);
});
