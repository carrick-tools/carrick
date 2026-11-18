/**
 * A wait on the compiler engine is bounded and carries evidence (carrick#4781).
 *
 * Three CI runs lost a whole shard to a test whose engine child went silent:
 * the wait had no limit, so the file ran to the shard's 10-minute kill with no
 * output. These tests give `boundedEngineWait` (and so `SidecarClient.send`) a
 * stand-in engine that never answers, and read what the failure says: the
 * test file, the request, the wait, whether the child was alive, the last
 * frame and stderr lines it wrote (an unfinished line too) and the JavaScript
 * stack it was stuck in. A test with no limit would hang to its own alarm.
 */
import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

/** A stand-in that writes 60 stderr lines and an unfinished one, a progress frame, then never answers. */
function standIn(dir: string, tail: string): string {
  const script = path.join(dir, 'silent.js');
  fs.writeFileSync(
    script,
    [
      "const fs = require('node:fs');",
      "for (let i = 1; i <= 60; i++) fs.writeSync(2, `log line ${i}\\n`);",
      "fs.writeSync(2, 'unfinished stderr text');",
      'fs.writeSync(1, JSON.stringify({ request_id: "init-x", status: "progress", phase: "program" }) + "\\n");',
      tail,
    ].join('\n')
  );
  return script;
}

describe('a bounded wait on the compiler engine', () => {
  // The node:test timeout is the alarm: a wait with no limit fails here.
  const alarm = { timeout: 60_000 };

  for (const [name, tail, mark] of [
    ['an engine in a synchronous loop', 'function spinForever() { for (;;) {} }\nspinForever();', 'spinForever'],
    ['an idle engine that ignores SIGTERM', "process.on('SIGTERM', () => {});\nprocess.stdin.resume();\nsetInterval(() => {}, 1000);", ''],
  ] as const) {
    it(`fails in seconds, killing ${name} and naming what it said`, alarm, async () => {
      const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-bounded-'));
      const client = new SidecarClient(standIn(dir, tail));
      try {
        await client.start();
        const pid = (client as unknown as { process: { pid: number } }).process.pid;
        const began = Date.now();
        await assert.rejects(client.send({ action: 'init', request_id: 'init-x' }, 700), (err: Error) => {
          const m = err.message;
          assert.match(m, /no answer to 'init' \(init-x\) within 700 ms/);
          assert.match(m, /test file: .*engine-bounded-read\.test\.js/);
          assert.match(m, /still alive/);
          assert.match(m, /"status":"progress"/, 'the last frame it sent');
          assert.match(m, /unfinished stderr text/, 'a stderr line with no newline');
          assert.match(m, /log line 60\n/);
          assert.ok(!m.includes('log line 5\n'), 'only the last 40 stderr lines');
          assert.match(m, /log line 22\n/);
          assert.ok(!m.includes('log line 21\n'), 'exactly the last 40 lines (39 whole and the unfinished one)');
          if (mark) assert.match(m, new RegExp(`at ${mark} \\(`), `the stack names ${mark}:\n${m}`);
          return true;
        });
        assert.ok(Date.now() - began < 15_000, 'it failed within seconds');
        // SIGKILL is delivered at once; the process table clears a moment later.
        for (let i = 0; i < 40 && alive(pid); i++) await new Promise((resolve) => setTimeout(resolve, 50));
        assert.strictEqual(alive(pid), false, 'the engine is killed when the wait fails');
      } finally {
        await client.stop();
        fs.rmSync(dir, { recursive: true, force: true });
      }
    });
  }
});
