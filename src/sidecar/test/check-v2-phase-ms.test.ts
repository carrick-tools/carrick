/**
 * v2 check core: the answer names how long each phase took (carrick#1047, J0).
 *
 * The timings are for the scanner's log only; no verdict reads them.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runCheck } from '../src/capture/index.js';
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

const PAIRS: CheckPairSpec[] = [
  {
    pair_key: 'pair-a',
    protocol: 'http',
    type_kind: 'response',
    producer: { service_name: 'orders', alias: 'P_Res' },
    consumer: { service_name: 'web', alias: 'C_Res' },
  },
];

describe('check_v2: phase timings', () => {
  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-check-v2-phase-'));
    writeStub('orders', 'export type P_Res = { a: string; };\n');
    writeStub('web', 'export type C_Res = { a: string; };\n');
    stubs = [
      { service_name: 'orders', stub_dir: path.join(root, 'orders') },
      { service_name: 'web', stub_dir: path.join(root, 'web') },
    ];
  });

  after(() => fs.rmSync(root, { recursive: true, force: true }));

  it('a successful check returns all six phases as non-negative integers', async () => {
    const result = await runCheck({ stubs, pairs: PAIRS });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    const phases = result.phase_ms;
    assert.ok(phases, 'phase_ms missing');
    assert.deepStrictEqual(Object.keys(phases).sort(), [
      'assemble',
      'classify',
      'install',
      'probe_program',
      'probes',
      'tsc',
    ]);
    for (const [k, v] of Object.entries(phases)) {
      assert.ok(Number.isInteger(v) && v >= 0, `${k}: ${v}`);
    }
  });

  it('an install failure returns no timings', async () => {
    const pnpmPath = path.join(root, 'pnpm-fail');
    fs.writeFileSync(pnpmPath, '#!/bin/sh\nexit 1\n');
    fs.chmodSync(pnpmPath, 0o755);
    const result = await runCheck({ stubs, pairs: PAIRS, pnpmPath });
    assert.strictEqual(result.success, false);
    assert.strictEqual(result.phase_ms, undefined);
  });
});
