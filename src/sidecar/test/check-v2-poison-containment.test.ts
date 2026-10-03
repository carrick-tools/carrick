/**
 * #438 part 2: stub-tree poison is contained to the aliases whose closure
 * includes the poisoned file — a bad alias no longer masks every clean pair
 * in the same service. Service-wide degradation stays reserved for install
 * (isolation) failure.
 *
 * Real vendored pnpm + real tsc, hand-authored dependency-free stubs (as in
 * check-v2.test.ts). Fixtures are synthetic and generically named.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runCheck } from '../src/capture/index.js';
import type { CheckPairSpec, CheckStubInput, CheckVerdict } from '../src/capture/api.js';

let root: string;

interface StubFile {
  rel: string;
  text: string;
}

function writeStub(dir: string, serviceName: string, files: StubFile[]): void {
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
  for (const f of files) {
    const abs = path.join(stubDir, 'types', f.rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, f.text);
  }
}

function fakeBin(name: string, script: string): string {
  const file = path.join(root, name);
  fs.writeFileSync(file, `#!/bin/sh\n${script}\n`);
  fs.chmodSync(file, 0o755);
  return file;
}

function byKey(verdicts: CheckVerdict[]): Map<string, CheckVerdict> {
  return new Map(verdicts.map((v) => [v.pair_key, v]));
}

function mk(
  pair_key: string,
  producerAlias: string,
  consumerAlias: string
): CheckPairSpec {
  return {
    pair_key,
    protocol: 'http',
    type_kind: 'response',
    producer: { service_name: 'orders', alias: producerAlias },
    consumer: { service_name: 'web', alias: consumerAlias },
  };
}

const PAIRS: CheckPairSpec[] = [
  // Poisoned by a broken SURFACE line on the producer side.
  mk('surface-poison', 'Surface_Poisoned_Sent', 'Surface_Poisoned_Exp'),
  // Poisoned by a broken NESTED file the producer alias imports.
  mk('nested-poison', 'Nested_Poisoned_Sent', 'Nested_Poisoned_Exp'),
  // Clean pair in the SAME producer service — must still verdict.
  mk('clean', 'Clean_Sent', 'Clean_Exp'),
];

let stubs: CheckStubInput[];

describe('check_v2: stub poison is contained to the affected aliases (#438 part 2)', () => {
  let verdicts: Map<string, CheckVerdict>;
  let degraded: string[];

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-poison-contain-'));
    writeStub(root, 'orders', [
      {
        rel: 'surface.d.ts',
        text: [
          // A value/type that does not exist -> TS2304 on THIS surface line.
          'export type Surface_Poisoned_Sent = NoSuchTypeAnywhere;',
          // Imports a broken nested file -> its closure is poisoned.
          "export type Nested_Poisoned_Sent = import('./schema').BadSchema;",
          // Fully clean, closure-independent.
          'export type Clean_Sent = { a: string; };',
        ].join('\n') + '\n',
      },
      {
        rel: 'schema.d.ts',
        // TS2304 in a nested file reachable only from Nested_Poisoned_Sent.
        text: 'export type BadSchema = AnotherMissingType;\n',
      },
    ]);
    writeStub(root, 'web', [
      {
        rel: 'surface.d.ts',
        text: [
          'export type Surface_Poisoned_Exp = { a: string; };',
          'export type Nested_Poisoned_Exp = { a: string; };',
          'export type Clean_Exp = { a: string; };',
        ].join('\n') + '\n',
      },
    ]);
    stubs = [
      { service_name: 'orders', stub_dir: path.join(root, 'orders') },
      { service_name: 'web', stub_dir: path.join(root, 'web') },
    ];
    const result = await runCheck({ stubs, pairs: PAIRS });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    verdicts = byKey(result.verdicts);
    degraded = result.degraded_services.map((d) => d.service_name);
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('the surface-line-poisoned pair is unverifiable (poison:producer)', () => {
    const v = verdicts.get('surface-poison')!;
    assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
    assert.strictEqual(v.gate, 'poison:producer');
  });

  it('the nested-file-poisoned pair is unverifiable (poison:producer)', () => {
    const v = verdicts.get('nested-poison')!;
    assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
    assert.strictEqual(v.gate, 'poison:producer');
  });

  it('the CLEAN pair in the same service still verdicts (containment)', () => {
    // Pre-fix this read `unverifiable` (poison:producer) because one bad alias
    // poisoned the whole service. Contained, it must reach a real verdict.
    const v = verdicts.get('clean')!;
    assert.strictEqual(v.bucket, 'compatible', JSON.stringify(v));
  });

  it('contained poison does not degrade the whole service', () => {
    assert.ok(
      !degraded.includes('orders'),
      `alias-scoped poison must not mark the service degraded: ${degraded.join(', ')}`
    );
  });
});

describe('check_v2: install failure still degrades service-wide (#438 part 2)', () => {
  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-poison-install-'));
    writeStub(root, 'orders', [
      { rel: 'surface.d.ts', text: 'export type P_Res = { a: string; };\n' },
    ]);
    writeStub(root, 'web', [
      { rel: 'surface.d.ts', text: 'export type C_Res = { a: string; };\n' },
    ]);
    stubs = [
      { service_name: 'orders', stub_dir: path.join(root, 'orders') },
      { service_name: 'web', stub_dir: path.join(root, 'web') },
    ];
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('a failing install marks every pair unverifiable and every service degraded', async () => {
    const pnpmPath = fakeBin('pnpm-fail', 'echo "boom" >&2; exit 1');
    const pairs: CheckPairSpec[] = [mk('only', 'P_Res', 'C_Res')];
    const result = await runCheck({ stubs, pairs, pnpmPath });
    assert.strictEqual(result.install_ok, false);
    assert.strictEqual(result.success, false);
    for (const v of result.verdicts) {
      assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
      assert.strictEqual(v.gate, 'install:failed');
    }
    const degraded = result.degraded_services.map((d) => d.service_name).sort();
    assert.deepStrictEqual(degraded, ['orders', 'web']);
  });

  // carrick#1821: the scanner reads why a check failed from `errors`, so the
  // installer's own output must be there, or every pair reads "no detail".
  it('a failing install names the installer output in errors', async () => {
    const pnpmPath = fakeBin(
      'pnpm-fail-detail',
      'echo "ERR_FAKE_FETCH GET https://registry.example/missing-pkg error (EPERM)" >&2; exit 1'
    );
    const pairs: CheckPairSpec[] = [mk('only', 'P_Res', 'C_Res')];
    const result = await runCheck({ stubs, pairs, pnpmPath });
    assert.strictEqual(result.install_ok, false);
    assert.strictEqual(result.errors.length, 1, JSON.stringify(result.errors));
    assert.match(result.errors[0], /^workspace dependency install failed: /);
    assert.match(result.errors[0], /ERR_FAKE_FETCH GET https:\/\/registry\.example\/missing-pkg error \(EPERM\)/);
  });

  // The installer prints the workspace by its real path. Where the scratch
  // root sits behind a symlink (the OS temp dir on macOS), scrubbing only the
  // path the check was given left the link's target in the message.
  it('a failing install names no part of the workspace path, given or real', async () => {
    const real = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-real-root-')));
    const link = path.join(root, 'linked-root');
    fs.symlinkSync(real, link);
    try {
      const pnpmPath = fakeBin(
        'pnpm-fail-path',
        'echo "ERR_FAKE_DIR at $(pwd -P)/packages/orders" >&2; exit 1'
      );
      const pairs: CheckPairSpec[] = [mk('only', 'P_Res', 'C_Res')];
      const result = await runCheck({ stubs, pairs, pnpmPath, workspaceRoot: link });
      assert.deepStrictEqual(result.errors, [
        'workspace dependency install failed: ERR_FAKE_DIR at /packages/orders',
      ]);
    } finally {
      fs.rmSync(real, { recursive: true, force: true });
    }
  });
});
