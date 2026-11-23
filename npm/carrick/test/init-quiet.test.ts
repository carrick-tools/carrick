// `carrick init` prints its progress line and its step results, and nothing
// else: a log line from the fact fetch (the hooks' logger writes to stderr)
// must not land in the middle of the progress line.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fetchFactsQuietly } from "../src/init/run.ts";
import { createLogger } from "../src/log.ts";

/** A workspace the fact fetch can read a project from, as the facts tests build one. */
function workspace(t: { after: (fn: () => void) => void }): string {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-init-quiet-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(
    path.join(root, ".carrick", "hosted", "snapshot.json"),
    JSON.stringify({ resolution: { repos: [{ project_slug: "demo-shop" }] } }),
  );
  return root;
}

/** What `work` writes to the process's stdout and stderr. */
async function captured(work: () => Promise<unknown>): Promise<string> {
  const written: string[] = [];
  const stdout = process.stdout.write;
  const stderr = process.stderr.write;
  const take = ((chunk: unknown) => {
    written.push(String(chunk));
    return true;
  }) as typeof process.stdout.write;
  process.stdout.write = take;
  process.stderr.write = take;
  try {
    await work();
  } finally {
    process.stdout.write = stdout;
    process.stderr.write = stderr;
  }
  return written.join("");
}

test("a fact fetch that finds the cloud still building prints nothing during init", async (t) => {
  const root = workspace(t);
  const building = (async () =>
    new Response(JSON.stringify({ success: false, error: { code: "facts_building", message: "building", retriable: true } }), {
      status: 409,
    })) as typeof fetch;
  const env = { ...process.env, CARRICK_TOKEN: "t", CARRICK_NO_FACTS_REFRESH: "" };
  const options = { request: building, env, sleep: async () => undefined, pauseMs: 1, waitMs: 3 };
  delete process.env["CARRICK_LOG_QUIET"];
  let outcome: unknown;
  const printed = await captured(async () => {
    outcome = await fetchFactsQuietly(root, options);
  });
  assert.equal(outcome, "retry", "the fetch did hit the retry path");
  assert.equal(printed, "");
  assert.equal(process.env["CARRICK_LOG_QUIET"], undefined, "the switch is put back after the run");
});

test("the hooks' logger still prints outside init", async () => {
  delete process.env["CARRICK_LOG_QUIET"];
  const printed = await captured(async () => createLogger("carrick-facts")("still building"));
  assert.match(printed, /carrick-facts: still building/);
});
