import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { binary, check, timeoutMs, touch } from "../src/cli.ts";
import { fakeBin, fakeEnv, fixturePath, makeWorkspace } from "./helpers.ts";

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
  const call = JSON.parse(fs.readFileSync(argvLog, "utf8").trim()) as { argv: string[] };
  assert.deepEqual(call.argv, ["check", "user-service/src/routes/users.ts", "--json"]);
});

test("touch with no file asks about the workspace", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const argvLog = path.join(workspace.root, "argv.log");

  const outcome = await touch(null, {
    cwd: workspace.root,
    env: fakeEnv({
      CARRICK_FAKE_ARGV_LOG: argvLog,
      CARRICK_FAKE_FIXTURE: fixturePath("touch-workspace.json"),
    }),
    bin: fakeBin,
  });
  assert.equal(outcome.result?.changed_since_index, 7);
  const call = JSON.parse(fs.readFileSync(argvLog, "utf8").trim()) as { argv: string[] };
  assert.deepEqual(call.argv, ["touch", "--json"]);
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
