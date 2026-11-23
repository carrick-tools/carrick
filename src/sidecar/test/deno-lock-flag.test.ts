/**
 * A Deno repo with no lockfile is read with `--no-lock`: `--frozen` refuses to
 * create one, so `deno info` fails on any `npm:` import. A lockfile at the
 * workspace root (members share it) or beside the service keeps `--frozen`.
 */
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { denoLockFlag } from '../src/capture/deno-project.js';

describe('denoLockFlag', () => {
  it('reads --no-lock when no lockfile covers the project', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'deno-lock-'));
    try {
      fs.mkdirSync(path.join(root, 'apps/api'), { recursive: true });
      assert.equal(denoLockFlag(root, path.join(root, 'apps/api')), '--no-lock');
    } finally { fs.rmSync(root, { recursive: true, force: true }); }
  });

  it('keeps --frozen for the workspace root lockfile and for the service own', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'deno-lock-'));
    try {
      fs.mkdirSync(path.join(root, 'apps/api'), { recursive: true });
      fs.writeFileSync(path.join(root, 'deno.lock'), '{}');
      assert.equal(denoLockFlag(root, path.join(root, 'apps/api')), '--frozen');
      fs.rmSync(path.join(root, 'deno.lock'));
      fs.writeFileSync(path.join(root, 'apps/api/deno.lock'), '{}');
      assert.equal(denoLockFlag(root, path.join(root, 'apps/api')), '--frozen');
    } finally { fs.rmSync(root, { recursive: true, force: true }); }
  });
});
