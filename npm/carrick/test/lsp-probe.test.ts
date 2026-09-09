// The LSP probe script, run the way a person runs it.
//
// It is the first step of every editor row in plugin/TEST-PLAN.md, so what
// matters is that its output says which file carries which diagnostics, and
// that its exit code separates "the server answered" from "it did not".
//
// Driven against the fake CLI, so no binary and no index: this asserts the
// probe, not the scanner.

import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import path from "node:path";
import { fakeEnv, makeWorkspace, pluginDir } from "./helpers.ts";

type Run = { stdout: string; stderr: string; code: number | null };

function runProbe(args: string[], env: NodeJS.ProcessEnv): Promise<Run> {
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [path.join(pluginDir, "scripts", "lsp-probe.mjs"), ...args], {
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk: Buffer) => (stdout += chunk.toString("utf8")));
    child.stderr.on("data", (chunk: Buffer) => (stderr += chunk.toString("utf8")));
    child.on("close", (code) => resolve({ stdout, stderr, code }));
  });
}

test("it reports a row per published file, and the root the server chose", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  const run = await runProbe(
    ["--workspace", workspace.root, "--open", workspace.file],
    fakeEnv({ CARRICK_LOG_QUIET: "0" }),
  );

  assert.equal(run.code, 0, run.stderr);
  // The opened file, and a counterpart the client never opened.
  assert.match(run.stdout, /user-service\/src\/routes\/users\.ts {2}3 diagnostics/);
  assert.match(run.stdout, /order-service\/src\/clients\/users\.ts/);
  assert.match(run.stdout, /codes: .*boundary/);
  assert.match(run.stdout, /related: 2, all on disk/);
  assert.match(run.stdout, /^OK 3 file\(s\) published$/m);
  // The root guard has no other observable effect, so the probe's job is to put
  // the line where the person running it can see it.
  assert.match(run.stderr, /\[server\].*initialized by carrick lsp-probe, root/);
});

test("a server that answers nothing is a non-zero exit, not an empty report", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  const run = await runProbe(
    ["--workspace", workspace.root, "--open", workspace.file, "--timeout", "2000"],
    fakeEnv({ CARRICK_FAKE_EXIT: "3" }),
  );

  assert.equal(run.code, 1);
  assert.match(run.stderr, /FAIL the server published nothing within 2000 ms/);
});

test("--json prints the publishes themselves, for diffing two runs", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());

  const run = await runProbe(
    ["--workspace", workspace.root, "--open", workspace.file, "--json"],
    fakeEnv(),
  );

  assert.equal(run.code, 0, run.stderr);
  const parsed = JSON.parse(run.stdout.slice(0, run.stdout.lastIndexOf("}") + 1)) as {
    workspace: string;
    publishes: Array<{ uri: string; diagnostics: unknown[] }>;
  };
  assert.equal(parsed.publishes.length, 3);
  assert.ok(parsed.publishes.every((publish) => publish.uri.startsWith("file://")));
});

test("a missing workspace or file is a usage error, before anything is spawned", async () => {
  const missing = await runProbe(["--workspace", "/nonexistent-carrick-probe", "--open", "x.ts"], fakeEnv());
  assert.equal(missing.code, 2);
  assert.match(missing.stderr, /no workspace at/);

  const noArgs = await runProbe([], fakeEnv());
  assert.equal(noArgs.code, 2);
  assert.match(noArgs.stderr, /--workspace is required/);
});
