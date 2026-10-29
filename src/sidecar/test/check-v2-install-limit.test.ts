/**
 * v2 check core: the workspace install has its own time limit (carrick#3021).
 *
 * A package manager that cannot reach the registry retries its fetches for
 * minutes, or for ever behind a dropped network. The check must come back
 * inside its limit, say why, and mark every pair as not checked.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runCheck } from '../src/capture/index.js';
import { installFailure, installTotalLimitMs, DEFAULT_INSTALL_TIMEOUT_MS } from '../src/capture/check.js';
import type { CheckPairSpec, CheckStubInput } from '../src/capture/api.js';

let root: string;
let stubs: CheckStubInput[];

function writeStub(name: string, surface: string): void {
  const dir = path.join(root, name);
  fs.mkdirSync(path.join(dir, 'types'), { recursive: true });
  fs.writeFileSync(
    path.join(dir, 'package.json'),
    JSON.stringify({ name: `@carrick/${name}`, version: '0.0.0-carrick', private: true, types: './types/surface.d.ts' })
  );
  fs.writeFileSync(path.join(dir, 'types', 'surface.d.ts'), surface);
}

function fakePnpm(name: string, script: string): string {
  const file = path.join(root, name);
  fs.writeFileSync(file, `#!/bin/sh\n${script}\n`);
  fs.chmodSync(file, 0o755);
  return file;
}

const PAIRS: CheckPairSpec[] = [
  {
    pair_key: 'pair-a',
    protocol: 'http',
    type_kind: 'response',
    producer: { service_name: 'orders', alias: 'P_Res' },
    consumer: { service_name: 'web', alias: 'C_Res' },
  },
];

describe('check_v2: the install is bounded and says why it stopped', () => {
  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-check-v2-install-'));
    writeStub('orders', 'export type P_Res = { a: string; };\n');
    writeStub('web', 'export type C_Res = { a: string; };\n');
    stubs = [
      { service_name: 'orders', stub_dir: path.join(root, 'orders') },
      { service_name: 'web', stub_dir: path.join(root, 'web') },
    ];
  });

  after(() => fs.rmSync(root, { recursive: true, force: true }));

  it('an install that hangs is killed at its limit and every pair reads not checked', async () => {
    // The child forks a sleeper: the kill must take the whole group.
    const pnpmPath = fakePnpm('pnpm-hang', 'sleep 600 & wait');
    const started = Date.now();
    const result = await runCheck({ stubs, pairs: PAIRS, pnpmPath, installTimeoutMs: 1500 });
    assert.ok(Date.now() - started < 20_000, 'returned long after the limit');
    assert.strictEqual(result.success, false);
    assert.strictEqual(result.install_ok, false);
    assert.strictEqual(result.verdicts.length, PAIRS.length);
    for (const v of result.verdicts) {
      assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
      assert.strictEqual(v.gate, 'install:timeout');
      assert.match(v.diagnostic ?? '', /did not finish within 2s/);
    }
    assert.match(result.errors.join('\n'), /did not finish within 2s/);
  });

  it('a silent install is killed at the idle limit, well before the total cap', async () => {
    const pnpmPath = fakePnpm('pnpm-silent', 'echo started; sleep 600 & wait');
    const started = Date.now();
    const result = await runCheck({
      stubs,
      pairs: PAIRS,
      pnpmPath,
      installIdleTimeoutMs: 1500,
      installTimeoutMs: 600_000,
    });
    assert.ok(Date.now() - started < 20_000, 'returned long after the idle limit');
    for (const v of result.verdicts) {
      assert.strictEqual(v.gate, 'install:timeout');
      assert.match(v.diagnostic ?? '', /printed nothing for 2s/);
    }
  });

  it('a slow install that keeps printing is not killed by the idle limit', async () => {
    const pnpmPath = fakePnpm(
      'pnpm-slow-chatty',
      'for i in 1 2 3 4 5 6; do echo progress $i; sleep 0.5; done; exit 1'
    );
    const result = await runCheck({ stubs, pairs: PAIRS, pnpmPath, installIdleTimeoutMs: 1200 });
    for (const v of result.verdicts) assert.strictEqual(v.gate, 'install:failed');
  });

  it('the total cap comes from the option, then the environment, then the default', () => {
    assert.strictEqual(installTotalLimitMs(5, { CARRICK_CHECK_INSTALL_TIMEOUT_MS: '9' }), 5);
    assert.strictEqual(installTotalLimitMs(undefined, { CARRICK_CHECK_INSTALL_TIMEOUT_MS: '9000' }), 9000);
    assert.strictEqual(installTotalLimitMs(undefined, { CARRICK_CHECK_INSTALL_TIMEOUT_MS: 'abc' }), DEFAULT_INSTALL_TIMEOUT_MS);
    assert.strictEqual(installTotalLimitMs(undefined, {}), DEFAULT_INSTALL_TIMEOUT_MS);
  });

  it('an install that cannot reach the registry names that, not a bare failure', async () => {
    const pnpmPath = fakePnpm(
      'pnpm-offline',
      "echo ' ERR_PNPM_META_FETCH_FAIL  GET https://registry.example/zod: getaddrinfo ENOTFOUND registry.example' >&2; exit 1"
    );
    const result = await runCheck({ stubs, pairs: PAIRS, pnpmPath });
    assert.strictEqual(result.success, false);
    for (const v of result.verdicts) {
      assert.strictEqual(v.bucket, 'unverifiable');
      assert.strictEqual(v.gate, 'install:registry-unreachable');
      assert.match(v.diagnostic ?? '', /registry could not be reached/);
    }
  });

  it('any other install failure keeps the plain gate', () => {
    assert.strictEqual(
      installFailure({ timedOut: false, stderr: 'ERR_PNPM_PEER_DEP_ISSUES', stdout: '' }, { idleMs: 1000, totalMs: 1000 }).gate,
      'install:failed'
    );
  });
});
