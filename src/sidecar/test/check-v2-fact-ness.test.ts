/**
 * carrick#707 R1d: the check phase's fact-ness walk.
 *
 * The probe gates prove neither side is WHOLLY `any`/`unknown`/`never`. They
 * cannot see a member three levels down, and `any` there is bidirectionally
 * assignable — so a producer declaring `{ id: string; meta: any }` clears every
 * gate and then reads compatible against a consumer expecting anything at all.
 * That is the absence of a comparison, not the result of one.
 *
 * The capture-time walk catches most of it, but by design not the case where a
 * member decays through a pinned external: at capture time that is
 * TypeScript's `error` placeholder, excluded there because installing the pin
 * heals it (carrick#450). This walk runs on the assembled workspace AFTER that
 * install, so it sees what the capture could not.
 *
 * These cases drive the module directly against a probes package on disk — no
 * pnpm, no network — because what is under test is whether the walk resolves
 * the probe's two imported surface aliases through real declaration files and
 * reports what it finds. The end-to-end wiring is covered by the check-phase
 * families in capture-v2-deep-any.test.ts.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { buildProbe } from '../src/capture/check-probe.js';
import { probeDeepFindings } from '../src/capture/check-deep.js';
import type { CheckPairSpec } from '../src/capture/api.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: {
    strict: true,
    skipLibCheck: false,
    noEmit: true,
    module: 'esnext',
    moduleResolution: 'bundler',
    target: 'es2022',
    baseUrl: '.',
    paths: {
      '@carrick/producer': ['./surfaces/producer.d.ts'],
      '@carrick/consumer': ['./surfaces/consumer.d.ts'],
    },
  },
  include: ['probes', 'surfaces'],
});

function spec(pairKey: string): CheckPairSpec {
  return {
    pair_key: pairKey,
    protocol: 'http',
    type_kind: 'response',
    producer: { service_name: 'producer', alias: `${pairKey}_Producer` },
    consumer: { service_name: 'consumer', alias: `${pairKey}_Consumer` },
  };
}

describe('carrick#707 R1d check-time fact-ness walk', () => {
  let dir: string;

  const PAIRS = ['clean', 'deep', 'nested'] as const;

  before(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-707-fact-'));
    fs.mkdirSync(path.join(dir, 'probes'), { recursive: true });
    fs.mkdirSync(path.join(dir, 'surfaces'), { recursive: true });
    fs.writeFileSync(path.join(dir, 'tsconfig.json'), TSCONFIG);

    // The producer surface: one clean alias, one carrying a member-level `any`
    // the whole-type gates cannot see, one carrying it under an array element.
    fs.writeFileSync(
      path.join(dir, 'surfaces', 'producer.d.ts'),
      `export type clean_Producer = { id: string; total: number };
export type deep_Producer = { id: string; meta: any };
export type nested_Producer = { rows: Array<{ id: string; payload: any }> };
`
    );
    fs.writeFileSync(
      path.join(dir, 'surfaces', 'consumer.d.ts'),
      `export type clean_Consumer = { id: string; total: number };
export type deep_Consumer = { id: string; meta: { owner: string } };
export type nested_Consumer = { rows: Array<{ id: string; payload: { at: string } }> };
`
    );

    for (const key of PAIRS) {
      const plan = buildProbe(spec(key), (s) => `@carrick/${s}`);
      fs.writeFileSync(path.join(dir, 'probes', plan.fileName), plan.source);
    }
  });

  after(() => {
    fs.rmSync(dir, { recursive: true, force: true });
  });

  function findingsFor(key: (typeof PAIRS)[number]) {
    const plan = buildProbe(spec(key), (s) => `@carrick/${s}`);
    const all = probeDeepFindings(dir, [plan]);
    return all.get(plan.pairId);
  }

  it('reports nothing for two fully known types', () => {
    const found = findingsFor('clean');
    assert.ok(found, 'the walk must run and resolve both aliases');
    assert.deepStrictEqual(found.sent, []);
    assert.deepStrictEqual(found.expected, []);
  });

  it('finds a member-level any that every whole-type gate passes', () => {
    const found = findingsFor('deep');
    assert.ok(found);
    assert.strictEqual(found.sent.length, 1);
    assert.strictEqual(found.sent[0].path, 'meta');
    assert.strictEqual(found.sent[0].kind, 'any');
    assert.strictEqual(found.sent[0].reason, 'declared');
    assert.ok(found.sent[0].detail, 'a finding without a reason is the bug this closes');
    // The consumer states a real shape, so only one side is at fault.
    assert.deepStrictEqual(found.expected, []);
  });

  it('finds an any buried under an array element', () => {
    const found = findingsFor('nested');
    assert.ok(found);
    assert.strictEqual(found.sent.length, 1);
    assert.match(found.sent[0].path, /payload$/);
    assert.strictEqual(found.sent[0].kind, 'any');
  });

  it('returns no entry rather than an empty one when it cannot run', () => {
    const empty = probeDeepFindings(path.join(dir, 'nowhere'), [
      buildProbe(spec('clean'), (s) => `@carrick/${s}`),
    ]);
    assert.strictEqual(
      empty.size,
      0,
      'a walk that could not run must be distinguishable from a clean one'
    );
  });
});
