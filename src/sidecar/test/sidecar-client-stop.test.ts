/**
 * SidecarClient.stop must end a sidecar that ignores SIGTERM (carrick#4781):
 * one stuck in a synchronous loop never runs its handler, and a live child
 * keeps the test process, and so the CI shard, from exiting.
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

describe('SidecarClient.stop', () => {
  it('kills a sidecar that ignores SIGTERM and names the wait that failed', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-stop-'));
    const script = path.join(dir, 'stuck.js');
    fs.writeFileSync(script, "process.on('SIGTERM', () => {});\nprocess.stdin.resume();\nsetInterval(() => {}, 1000);\n");
    const client = new SidecarClient(script);
    try {
      await client.start();
      const pid = (client as unknown as { process: { pid: number } }).process.pid;
      await assert.rejects(client.send({ action: 'init', request_id: 'init-x' }, 300), /no answer to 'init' \(init-x\) within 300 ms/);
      const began = Date.now();
      await client.stop(500);
      assert.ok(Date.now() - began < 15000, 'stop is bounded');
      assert.strictEqual(alive(pid), false, 'the sidecar is gone when stop returns');
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
});
