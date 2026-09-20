/**
 * v2 check core — abnormal tsc termination (adversarial-review finding 3).
 *
 * runCheck used to ignore the tsc exit code entirely: an OOM/SIGKILL
 * (code null), an internal crash, or a fileless config error (TS18003
 * prints WITHOUT a `(line,col)` prefix, so parseTscOutput yields zero
 * diagnostics) all fell through classifyPair's empty-diagnostics branch and
 * every pair read COMPATIBLE with success:true — the dangerous direction.
 *
 * These tests inject a stand-in tsc via CheckOptions.tscPath (the same
 * injection seam as pnpmPath) to pin:
 *  1. non-zero exit + zero parseable diagnostics -> every pair unverifiable,
 *     success:false, degraded services recorded;
 *  2. signal death (exit code null) -> same;
 *  3. non-zero exit WITH parseable probe diagnostics stays the legitimate
 *     incompatible path (tsc exits 1 on ordinary type errors — the guard
 *     must NOT be "fail on non-zero").
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runCheck } from '../src/capture/index.js';
import { buildProbe, decisiveAssignmentLine } from '../src/capture/check-probe.js';
import type { CheckPairSpec, CheckStubInput } from '../src/capture/api.js';

let root: string;
let stubs: CheckStubInput[];

function writeStub(dir: string, serviceName: string, surface: string): void {
  const stubDir = path.join(dir, serviceName);
  fs.mkdirSync(path.join(stubDir, 'types'), { recursive: true });
  fs.writeFileSync(
    path.join(stubDir, 'package.json'),
    JSON.stringify(
      {
        name: `@carrick/${serviceName}`,
        version: '0.0.0-carrick',
        private: true,
        types: './types/surface.d.ts',
      },
      null,
      2
    ) + '\n'
  );
  fs.writeFileSync(path.join(stubDir, 'types', 'surface.d.ts'), surface);
}

function fakeTsc(name: string, script: string): string {
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
  {
    pair_key: 'pair-b',
    protocol: 'http',
    type_kind: 'request',
    producer: { service_name: 'orders', alias: 'P_Req' },
    consumer: { service_name: 'web', alias: 'C_Req' },
  },
];

describe('check_v2: abnormal tsc termination is never read as compatible', () => {
  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-check-v2-tsc-'));
    writeStub(
      root,
      'orders',
      'export type P_Res = { a: string; };\nexport type P_Req = { a: string; };\n'
    );
    writeStub(
      root,
      'web',
      'export type C_Res = { a: string; };\nexport type C_Req = { a: string; };\n'
    );
    stubs = [
      { service_name: 'orders', stub_dir: path.join(root, 'orders') },
      { service_name: 'web', stub_dir: path.join(root, 'web') },
    ];
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('crash exit with no parseable diagnostics -> all pairs unverifiable', async () => {
    const tscPath = fakeTsc(
      'tsc-crash',
      "echo 'error TS18003: No inputs were found in config file.' >&2; exit 1"
    );
    const result = await runCheck({ stubs, pairs: PAIRS, tscPath });
    assert.strictEqual(result.success, false);
    assert.strictEqual(result.install_ok, true);
    assert.strictEqual(result.verdicts.length, PAIRS.length);
    for (const v of result.verdicts) {
      assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
      assert.strictEqual(v.gate, 'tsc:abnormal-termination');
      assert.match(v.diagnostic ?? '', /terminated abnormally/);
    }
    assert.strictEqual(result.degraded_services.length, 2);
    assert.match(result.errors.join('\n'), /exit code 1/);
    assert.match(result.errors.join('\n'), /TS18003/);
  });

  it('signal death (exit code null, e.g. OOM SIGKILL) -> all pairs unverifiable', async () => {
    const tscPath = fakeTsc('tsc-sigkill', 'kill -KILL $$');
    const result = await runCheck({ stubs, pairs: PAIRS, tscPath });
    assert.strictEqual(result.success, false);
    for (const v of result.verdicts) {
      assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
      assert.strictEqual(v.gate, 'tsc:abnormal-termination');
    }
    assert.match(result.errors.join('\n'), /exit code null/);
  });

  it('non-zero exit WITH parseable probe diagnostics stays the incompatible path', async () => {
    // Fake tsc emits a real assignment-class diagnostic for pair-a's probe
    // and exits 1 exactly like the real tsc does on ordinary type errors.
    // The abnormal-termination guard must not swallow it.
    const plan = buildProbe(PAIRS[0], (s) => `@carrick/${s}`);
    const probeFile = `packages/carrick-probes/probes/${plan.fileName}`;
    const tscPath = fakeTsc(
      'tsc-type-error',
      `echo "${probeFile}(${decisiveAssignmentLine(plan)},7): error TS2322: Type 'A' is not assignable to type 'B'."; exit 1`
    );
    const result = await runCheck({ stubs, pairs: PAIRS, tscPath });
    assert.strictEqual(result.success, true);
    const verdictA = result.verdicts.find((v) => v.pair_key === 'pair-a')!;
    assert.strictEqual(verdictA.bucket, 'incompatible', JSON.stringify(verdictA));
    const verdictB = result.verdicts.find((v) => v.pair_key === 'pair-b')!;
    assert.strictEqual(verdictB.bucket, 'compatible', JSON.stringify(verdictB));
  });
});
