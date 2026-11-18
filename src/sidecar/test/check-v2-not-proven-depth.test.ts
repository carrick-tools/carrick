/**
 * Incompatible-rule T1: the relaxed re-check looks at every level it relaxes.
 *
 * Past a few levels of the same rebuilt type on each side, the compiler stops
 * comparing and assumes two types related. A relaxed check that stopped
 * there would read a field the receiver requires, three arrays down, as not
 * proven. So at each depth from 1 to 9, under arrays, record values, nullable
 * objects, and a sent side whose wire form is itself rebuilt (it holds a
 * Date), each shape is judged twice beside an open value at the root: with a
 * field missing at the leaf (proven) and with the leaf agreeing (not proven,
 * open value, or proven once the leaf sits below the levels the re-check
 * reads). Also under `Partial`, `Readonly` and `Pick`. Through `runCheck`, on the judge's compiler and on
 * TypeScript 6, the fallback where the native one is not installed.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runCheck } from '../src/capture/index.js';
import { resolveVendored } from '../src/capture/check.js';
import { RELAXED_DEPTH, REMAINS } from '../src/capture/check-relaxed.js';
import type { CheckPairSpec, CheckResult, CheckStubInput } from '../src/capture/api.js';

/** Each shape, and how many levels of the re-check one use of it takes. */
const SHAPES: Record<string, { wrap: (inner: string) => string; levels: number }> = {
  array: { wrap: (x) => `{ n: ${x}[] }`, levels: 2 },
  record: { wrap: (x) => `{ n: Record<string, ${x}> }`, levels: 2 },
  nullable: { wrap: (x) => `{ n: ${x} | null }`, levels: 1 },
  partial: { wrap: (x) => `Partial<{ n: ${x}; m: string }>`, levels: 1 },
  readonly: { wrap: (x) => `Readonly<{ n: ${x} }>`, levels: 1 },
  picked: { wrap: (x) => `Pick<{ n: ${x}; m: string }, 'n'>`, levels: 1 },
};

interface Seed {
  key: string;
  producer: string;
  consumer: string;
  proven: boolean;
}

function nest(shape: (inner: string) => string, depth: number, leaf: string): string {
  let t = leaf;
  for (let i = 0; i < depth; i++) t = shape(t);
  return t;
}

const SEEDS: Seed[] = [];
for (const [name, { wrap: shape, levels }] of Object.entries(SHAPES)) {
  for (let depth = 1; depth <= 9; depth++) {
    // The leaf's field sits at level 2 + levels * depth (the root, then `x`).
    const tooDeep = 2 + levels * depth >= RELAXED_DEPTH;
    for (const date of [false, true]) {
      const at = date ? { producer: ' at: Date;', consumer: ' at: string;' } : { producer: '', consumer: '' };
      const root = (open: boolean, inner: string, side: 'producer' | 'consumer') =>
        `{ kind: ${open ? 'string' : "'card' | 'wire'"};${at[side]} x: ${inner} }`;
      const sentLeaf = nest(shape, depth, '{ id: string }');
      const key = `${name}${depth}${date ? 'date' : ''}`;
      SEEDS.push({
        key: `${key}missing`,
        producer: root(true, sentLeaf, 'producer'),
        consumer: root(false, nest(shape, depth, '{ id: string; openedAt: string }'), 'consumer'),
        proven: true,
      });
      SEEDS.push({
        key: `${key}agrees`,
        producer: root(true, sentLeaf, 'producer'),
        consumer: root(false, nest(shape, depth, '{ id: string }'), 'consumer'),
        proven: tooDeep,
      });
    }
  }
}

const PAIRS: CheckPairSpec[] = SEEDS.map((s) => ({
  pair_key: s.key,
  protocol: 'http',
  type_kind: 'response',
  producer: { service_name: 'api', alias: `${s.key}_P` },
  consumer: { service_name: 'web', alias: `${s.key}_C` },
}));

function writeStub(root: string, name: string, surface: string): void {
  const dir = path.join(root, name);
  fs.mkdirSync(path.join(dir, 'types'), { recursive: true });
  fs.writeFileSync(
    path.join(dir, 'package.json'),
    JSON.stringify({ name: `@carrick/${name}`, version: '0.0.0-carrick', private: true, types: './types/surface.d.ts' })
  );
  fs.writeFileSync(path.join(dir, 'types', 'surface.d.ts'), surface);
}

describe('the relaxed re-check compares every level it relaxes (incompatible-rule T1)', () => {
  let root: string;
  let stubs: CheckStubInput[];

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-not-proven-depth-'));
    writeStub(root, 'api', SEEDS.map((s) => `export type ${s.key}_P = ${s.producer};`).join('\n') + '\n');
    writeStub(root, 'web', SEEDS.map((s) => `export type ${s.key}_C = ${s.consumer};`).join('\n') + '\n');
    stubs = [
      { service_name: 'api', stub_dir: path.join(root, 'api') },
      { service_name: 'web', stub_dir: path.join(root, 'web') },
    ];
  });

  after(() => fs.rmSync(root, { recursive: true, force: true }));

  function assertEvidence(result: CheckResult): void {
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    const byKey = new Map(result.verdicts.map((v) => [v.pair_key, v]));
    const wrong: string[] = [];
    for (const seed of SEEDS) {
      const v = byKey.get(seed.key)!;
      if (v.bucket !== 'incompatible') {
        wrong.push(`${seed.key}: judged ${v.bucket}`);
        continue;
      }
      const e = v.break_evidence!;
      const want = seed.proven
        ? { proven: true, kinds: [], passing: [], cause: REMAINS }
        : { proven: false, kinds: ['open_value'], passing: [['open_value']] };
      if (JSON.stringify(e) !== JSON.stringify(want)) wrong.push(`${seed.key}: ${JSON.stringify(e)}`);
    }
    assert.deepStrictEqual(wrong, []);
  }

  it('on the judge compiler', async () => {
    assertEvidence(await runCheck({ stubs, pairs: PAIRS }));
  });

  it('on TypeScript 6', async () => {
    assertEvidence(await runCheck({ stubs, pairs: PAIRS, tscPath: resolveVendored(path.join('typescript', 'bin', 'tsc')) }));
  });
});
