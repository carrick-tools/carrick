import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import { spawnSync } from "node:child_process";
import path from "node:path";
import { binary, check, status, timeoutMs } from "../src/cli.ts";
import { fakeBin, fakeEnv, firstCall, fixturePath, makeWorkspace } from "./helpers.ts";

test("the binary is `carrick` on PATH unless CARRICK_BIN names another", () => {
  assert.equal(binary({}), "carrick");
  assert.equal(binary({ CARRICK_BIN: "/opt/carrick" }), "/opt/carrick");
});

test("the CLI time limit defaults to five seconds and is configurable", () => {
  assert.equal(timeoutMs({}), 5000);
  assert.equal(timeoutMs({ CARRICK_TIMEOUT_MS: "250" }), 250);
  assert.equal(timeoutMs({ CARRICK_TIMEOUT_MS: "nonsense" }), 5000);
});

test("check runs the binary from the workspace root and parses its answer", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const argvLog = path.join(workspace.root, "argv.log");

  const outcome = await check("user-service/src/routes/users.ts", {
    cwd: workspace.root,
    env: fakeEnv({ CARRICK_FAKE_ARGV_LOG: argvLog }),
    bin: fakeBin,
  });
  assert.equal(outcome.failure, null);
  assert.equal(outcome.result?.service, "user-service");
  const call = firstCall(argvLog);
  assert.ok(call);
  assert.deepEqual(call.argv, ["check", "user-service/src/routes/users.ts", "--json"]);
});

test("a re-check asks for one and states its own limit", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const argvLog = path.join(workspace.root, "argv.log");

  // A re-check has no default limit: the caller sizes it above the binary's
  // own budget, or the call is killed and prints nothing (carrick#1036).
  const outcome = await check("user-service/src/routes/users.ts", {
    cwd: workspace.root,
    env: fakeEnv({ CARRICK_FAKE_ARGV_LOG: argvLog, CARRICK_FAKE_DELAY_MS: "600" }),
    bin: fakeBin,
    recheck: true,
    limitMs: 100,
  });
  assert.equal(outcome.result, null, "the stated limit ended the call");
  const longer = await check("user-service/src/routes/users.ts", {
    cwd: workspace.root,
    env: fakeEnv({ CARRICK_FAKE_ARGV_LOG: argvLog, CARRICK_FAKE_DELAY_MS: "600" }),
    bin: fakeBin,
    recheck: true,
    noRescan: true,
    limitMs: 10_000,
  });
  assert.equal(longer.failure, null);
  const call = firstCall(argvLog);
  assert.ok(call);
  assert.deepEqual(call.argv, [
    "check",
    "user-service/src/routes/users.ts",
    "--json",
    "--recheck",
  ]);
  const probeCall = fs.readFileSync(argvLog, "utf8").trim().split("\n").map((l) => JSON.parse(l) as { argv: string[] })[1];
  assert.deepEqual(probeCall?.argv, [
    "check",
    "user-service/src/routes/users.ts",
    "--json",
    "--recheck",
    "--no-rescan",
  ]);
});

test("status asks about the workspace and takes no file", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const argvLog = path.join(workspace.root, "argv.log");

  const outcome = await status({
    cwd: workspace.root,
    workspace: workspace.root,
    env: fakeEnv({
      CARRICK_FAKE_ARGV_LOG: argvLog,
      CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json"),
    }),
    bin: fakeBin,
  });
  assert.equal(outcome.result?.services.length, 3);
  assert.equal(outcome.result?.services[0]?.routes, 157);
  const call = firstCall(argvLog);
  assert.ok(call);
  assert.deepEqual(call.argv, ["status", "--workspace", workspace.root, "--json"]);
});

test("status without a named workspace lets the CLI find the index", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const argvLog = path.join(workspace.root, "argv.log");

  await status({
    cwd: workspace.root,
    workspace: null,
    env: fakeEnv({
      CARRICK_FAKE_ARGV_LOG: argvLog,
      CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json"),
    }),
    bin: fakeBin,
  });
  const call = firstCall(argvLog);
  assert.ok(call);
  assert.deepEqual(call.argv, ["status", "--json"]);
});

test("a check payload is not accepted as a status answer, or the other way round", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  const wrongWay = await status({
    cwd: workspace.root,
    workspace: null,
    env: fakeEnv({ CARRICK_FAKE_FIXTURE: fixturePath("check-mismatch.json") }),
    bin: fakeBin,
  });
  assert.equal(wrongWay.result, null);
  assert.match(wrongWay.failure ?? "", /printed no payload this reader knows/);

  const otherWay = await check("a.ts", {
    cwd: workspace.root,
    env: fakeEnv({ CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json") }),
    bin: fakeBin,
  });
  assert.equal(otherWay.result, null);
});

test("a failure is reported, never thrown", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  const missing = await check("a.ts", {
    cwd: workspace.root,
    env: fakeEnv(),
    bin: "/nonexistent/carrick",
  });
  assert.equal(missing.result, null);
  assert.match(missing.failure ?? "", /failed:/);

  const noisy = await check("a.ts", {
    cwd: workspace.root,
    env: fakeEnv({ CARRICK_FAKE_EXIT: "2" }),
    bin: fakeBin,
  });
  assert.equal(noisy.result, null);
  assert.ok(noisy.failure);
});

test("a CLI slower than the limit is killed", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  const outcome = await check("a.ts", {
    cwd: workspace.root,
    env: fakeEnv({ CARRICK_FAKE_DELAY_MS: "3000", CARRICK_TIMEOUT_MS: "200" }),
    bin: fakeBin,
  });
  assert.equal(outcome.result, null);
  assert.ok(outcome.ms < 2000, `gave up after ${outcome.ms}ms`);
});

// carrick-cloud#2884: `--help` is an answer, not an argument. `lsp` used to
// start the language server and `templates` refused `--help` as a name.
for (const command of ["lsp", "templates"]) {
  for (const flag of ["--help", "-h"]) {
    test(`carrick ${command} ${flag} prints its help and exits 0`, () => {
      const run = spawnSync(
        process.execPath,
        [path.join(import.meta.dirname, "..", "bin", "carrick.mjs"), command, flag],
        { encoding: "utf8", timeout: 10000, input: "" },
      );
      assert.equal(run.status, 0, run.stderr);
      assert.match(run.stdout, new RegExp(`^carrick ${command} `));
    });
  }
}
