/**
 * J1 (carrick#1047): a stub's global-scope files are loaded by every judge
 * run, so the verdict of a pair depends on its two sides and on every stub's
 * global files, never on which other pairs the run probes.
 *
 * Stub X's alias reads a shared type that stub Z augments. Z's surface pulls
 * its augmentation in. Before the fix Z was loaded only when a probe imported
 * it: a run that held a pair of Z judged X's pair with the augmentation, a run
 * that held none judged it without. Z's manifest records `globals_hash`, which
 * is what tells the judge Z has global-scope files.
 *
 * Real vendored pnpm and real tsc, hand-authored dependency-free stubs.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runCheck } from '../src/capture/index.js';
import type { CheckPairSpec, CheckStubInput, CheckVerdict } from '../src/capture/api.js';

const roots: string[] = [];

function writeStub(
  dir: string,
  service: string,
  files: Record<string, string>,
  manifest: Record<string, unknown> = {}
): CheckStubInput {
  const stubDir = path.join(dir, service);
  fs.mkdirSync(path.join(stubDir, 'types'), { recursive: true });
  fs.writeFileSync(
    path.join(stubDir, 'package.json'),
    JSON.stringify({ name: `@carrick/${service}`, version: '0.0.0-carrick', private: true, types: './types/surface.d.ts' }) + '\n'
  );
  fs.writeFileSync(path.join(stubDir, 'carrick-manifest.json'), JSON.stringify({ aliases: [], ...manifest }) + '\n');
  for (const [rel, text] of Object.entries(files)) fs.writeFileSync(path.join(stubDir, 'types', rel), text);
  return { service_name: service, stub_dir: stubDir };
}

function pair(key: string, producer: string, producerAlias: string, consumerAlias: string): CheckPairSpec {
  return {
    pair_key: key,
    protocol: 'http',
    type_kind: 'response',
    producer: { service_name: producer, alias: producerAlias },
    consumer: { service_name: 'web', alias: consumerAlias },
  };
}

interface Variant {
  name: string;
  x: Record<string, string>;
  z: Record<string, string>;
}

const VARIANTS: Variant[] = [
  {
    name: 'declare global',
    x: {
      'surface.d.ts': '/// <reference path="./shared.d.ts" />\nexport type X_Sent = Shared;\n',
      'shared.d.ts': 'interface Shared { a: string }\n',
    },
    z: {
      'surface.d.ts': "import './aug';\nexport type Z_Sent = { z: number };\n",
      'aug.d.ts': 'export {};\ndeclare global { interface Shared { b: number } }\n',
    },
  },
  {
    name: 'declare module',
    x: {
      'surface.d.ts':
        '/// <reference path="./shared.d.ts" />\nimport type { Shared } from "shared-mod";\nexport type X_Sent = Shared;\n',
      'shared.d.ts': 'declare module "shared-mod" { export interface Shared { a: string } }\n',
    },
    z: {
      'surface.d.ts': "import './aug';\nexport type Z_Sent = { z: number };\n",
      'aug.d.ts': 'export {};\ndeclare module "shared-mod" { interface Shared { b: number } }\n',
    },
  },
];

async function judgeXPair(variant: Variant, zHasPair: boolean): Promise<CheckVerdict> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-stub-globals-'));
  roots.push(root);
  const stubs = [
    writeStub(root, 'x', variant.x),
    writeStub(root, 'z', variant.z, { globals_hash: 'recorded-by-capture' }),
    writeStub(root, 'web', {
      'surface.d.ts': 'export type X_Exp = { a: string; b: number };\nexport type Z_Exp = { z: number };\n',
    }),
  ];
  const pairs = [pair('x-pair', 'x', 'X_Sent', 'X_Exp')];
  if (zHasPair) pairs.push(pair('z-pair', 'z', 'Z_Sent', 'Z_Exp'));
  const result = await runCheck({ stubs, pairs });
  assert.strictEqual(result.success, true, JSON.stringify(result.errors));
  return result.verdicts.find((v) => v.pair_key === 'x-pair')!;
}

describe('check_v2: every stub with global-scope files is loaded in every run (J1)', () => {
  after(() => {
    for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
  });

  for (const variant of VARIANTS) {
    it(`${variant.name}: a pair's verdict does not depend on whether another stub has a pair`, async () => {
      const withZ = await judgeXPair(variant, true);
      const withoutZ = await judgeXPair(variant, false);
      assert.strictEqual(withZ.bucket, 'compatible', JSON.stringify(withZ));
      assert.strictEqual(withoutZ.bucket, withZ.bucket, JSON.stringify(withoutZ));
    });
  }
});
