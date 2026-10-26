// The session-start hook warms the resident type daemon (carrick#2884): in
// the background, at most once across sessions opening together, and never
// when it is turned off.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { DAEMON_START_COOLDOWN_MS, warmDaemonInBackground } from "../src/hook/daemon.ts";

function workspace(): { root: string; cleanup: () => void } {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-daemon-"));
  fs.mkdirSync(path.join(root, ".carrick"));
  return { root, cleanup: () => fs.rmSync(root, { recursive: true, force: true }) };
}

test("two sessions opening together start the daemon once, in the background", async () => {
  const { root, cleanup } = workspace();
  try {
    const started: Array<{ command: string; args: string[]; env: NodeJS.ProcessEnv }> = [];
    const start = (command: string, args: string[], env: NodeJS.ProcessEnv): void => {
      started.push({ command, args, env });
    };
    const now = Date.parse("2026-10-26T18:00:00Z");
    const env = { CARRICK_BIN: "carrick" };
    const both = await Promise.all([
      Promise.resolve().then(() => warmDaemonInBackground(root, { now, start, env })),
      Promise.resolve().then(() => warmDaemonInBackground(root, { now, start, env })),
    ]);
    assert.deepEqual(both.filter(Boolean).length, 1);
    assert.equal(started.length, 1);
    assert.deepEqual(started[0]?.args, ["daemon", "start", "--warm", "--workspace", root]);
    assert.equal(started[0]?.env["CARRICK_NO_KEEP_AWAKE"], "1");

    // A session after the cooldown starts it again: the binary keeps one
    // daemon per workspace, and a fresh warm-up covers the changes since.
    assert.equal(
      warmDaemonInBackground(root, { now: now + DAEMON_START_COOLDOWN_MS + 1, start, env }),
      true,
    );
  } finally {
    cleanup();
  }
});

test("a daemon turned off is never started", () => {
  const { root, cleanup } = workspace();
  try {
    let calls = 0;
    const started = warmDaemonInBackground(root, {
      start: () => {
        calls += 1;
      },
      env: { CARRICK_BIN: "carrick", CARRICK_TYPED: "off" },
    });
    assert.equal(started, false);
    assert.equal(calls, 0);
  } finally {
    cleanup();
  }
});
