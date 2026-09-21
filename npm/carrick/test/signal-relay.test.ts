// What this package does with a signal meant for the scan it is running
// (carrick#1391).
//
// The defect was that it did nothing: Node's default action ended this
// process, the scanner was never told, and — holding pipes nobody read any
// more — it died on its next write with nothing said to the cloud. That is
// end-to-end behaviour of a process being signalled, so the last test here
// spawns one and signals it; the rest drive the relay with a host of their
// own, which is the only way to assert what it does with a second signal
// without ending the test runner.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { relaySignals } from "../src/scan.ts";

/** A stand-in for this process's signal handling, and for the child. */
function harness() {
  const handlers = new Map<NodeJS.Signals, Set<() => void>>();
  const killed: NodeJS.Signals[] = [];
  const raised: NodeJS.Signals[] = [];
  return {
    killed,
    raised,
    installed: (): NodeJS.Signals[] =>
      [...handlers.entries()].filter(([, set]) => set.size > 0).map(([signal]) => signal),
    send: (signal: NodeJS.Signals): void => {
      for (const handler of [...(handlers.get(signal) ?? [])]) handler();
    },
    child: {
      kill: (signal: NodeJS.Signals): boolean => {
        killed.push(signal);
        return true;
      },
    },
    host: {
      on: (signal: NodeJS.Signals, handler: () => void): void => {
        if (!handlers.has(signal)) handlers.set(signal, new Set());
        handlers.get(signal)?.add(handler);
      },
      off: (signal: NodeJS.Signals, handler: () => void): void => {
        handlers.get(signal)?.delete(handler);
      },
      raise: (signal: NodeJS.Signals): void => void raised.push(signal),
    },
  };
}

test("a SIGTERM reaches the scan, which a terminal's own signal never did", () => {
  const h = harness();
  relaySignals(h.child, h.host);
  assert.deepEqual(h.installed().sort(), ["SIGHUP", "SIGINT", "SIGTERM"]);
  h.send("SIGTERM");
  assert.deepEqual(h.killed, ["SIGTERM"]);
  // And nothing is raised here: this process stays for the child's own ending.
  assert.deepEqual(h.raised, []);
});

test("a SIGINT is waited out rather than sent on", () => {
  const h = harness();
  relaySignals(h.child, h.host);
  h.send("SIGINT");
  // A terminal has already delivered it to the whole process group, and a
  // second one tells the scan to abandon the report it is making.
  assert.deepEqual(h.killed, []);
  assert.deepEqual(h.raised, []);
});

test("a second signal ends it now, and takes the scan with it", () => {
  const h = harness();
  relaySignals(h.child, h.host);
  h.send("SIGINT");
  h.send("SIGINT");
  assert.deepEqual(h.killed, ["SIGKILL"]);
  assert.deepEqual(h.raised, ["SIGINT"]);
  // Nothing is left listening, or the raised signal would be caught here
  // instead of ending this process.
  assert.deepEqual(h.installed(), []);
});

test("the relay is undone before the caller reports the run", () => {
  const h = harness();
  const stop = relaySignals(h.child, h.host);
  stop();
  assert.deepEqual(h.installed(), []);
  h.send("SIGTERM");
  assert.deepEqual(h.killed, []);
});

test("a signalled run tells the scan and waits for it", { timeout: 60_000 }, async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-signal-"));
  const record = path.join(dir, "signal.txt");
  const driver = fileURLToPath(new URL("./signal-driver.mjs", import.meta.url));
  const fake = fileURLToPath(new URL("./fake-stubborn-scan.mjs", import.meta.url));

  const child = spawn(process.execPath, [driver, fake, record], {
    stdio: ["ignore", "ignore", "pipe"],
  });
  // The scan is up and its handlers are installed. Read off disk: this
  // process renders the child's stderr rather than passing it through, so
  // waiting on a stream here would wait for the end of the run.
  const upBy = Date.now() + 30_000;
  while (!fs.existsSync(`${record}.ready`)) {
    assert.ok(Date.now() < upBy, "the scan never started");
    await new Promise((resolve) => setTimeout(resolve, 20));
  }

  const ended = new Promise<{ code: number | null; signal: NodeJS.Signals | null }>(
    (resolve) => child.on("exit", (code, signal) => resolve({ code, signal })),
  );
  child.kill("SIGTERM");
  const outcome = await ended;

  assert.equal(
    fs.readFileSync(record, "utf8"),
    "SIGTERM",
    "the scan was never told to stop",
  );
  assert.equal(
    outcome.signal,
    null,
    "this process died of the signal instead of staying for the scan",
  );
  assert.equal(outcome.code, 143, "the scan's own exit code is this process's");
  fs.rmSync(dir, { recursive: true, force: true });
});
