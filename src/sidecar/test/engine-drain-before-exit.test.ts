/**
 * The engine must not exit while an answer is still in the stdout pipe.
 * A pipe takes 64 KiB at a time; `process.exit` straight after the write
 * cut a larger frame at exactly 65,536 bytes, so the reader saw no answer.
 *
 * The reader here starts reading only after the engine has had time to write
 * and exit, so the pipe is full when the exit is requested.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import { spawn } from 'node:child_process';
import { SIDECAR_PATH } from './helpers.js';

/** Well over one pipe buffer: the error frame echoes the request id. */
const ID_BYTES = 300 * 1024;
const BIG_ID = 'r'.repeat(ID_BYTES);

function runEngine(ending: 'shutdown' | 'stdin-close'): Promise<{ stdout: string; code: number | null }> {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [SIDECAR_PATH], { stdio: ['pipe', 'pipe', 'ignore'] });
    const chunks: Buffer[] = [];
    child.on('error', reject);
    child.on('close', (code) => resolve({ stdout: Buffer.concat(chunks).toString('utf8'), code }));
    // An invalid request (no action) is answered with an error frame that
    // carries the request id, so the frame is as large as the id.
    child.stdin.write(JSON.stringify({ request_id: BIG_ID }) + '\n');
    if (ending === 'shutdown') {
      child.stdin.write(JSON.stringify({ action: 'shutdown', request_id: 'bye' }) + '\n');
    } else {
      child.stdin.end();
    }
    child.stdout.pause();
    setTimeout(() => {
      child.stdout.on('data', (c: Buffer) => chunks.push(c));
      child.stdout.resume();
    }, 1500);
  });
}

describe('engine drains stdout before it exits', () => {
  for (const ending of ['shutdown', 'stdin-close'] as const) {
    it(`a ${ID_BYTES}-byte answer arrives whole before ${ending}`, async () => {
      const { stdout, code } = await runEngine(ending);
      const frames = stdout.split('\n').filter((l) => l.trim());
      const big = JSON.parse(frames[0]) as { request_id: string; status: string };
      assert.strictEqual(big.status, 'error');
      assert.strictEqual(big.request_id.length, ID_BYTES);
      assert.strictEqual(code, 0);
      if (ending === 'shutdown') {
        assert.strictEqual((JSON.parse(frames[1]) as { status: string }).status, 'success');
      }
    });
  }
});
