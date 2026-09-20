/**
 * carrick-tools/carrick-cloud#1118: the field-level report behind the mismatch
 * text.
 *
 * The end-to-end sentences are pinned in check-v2.test.ts against the real
 * pnpm + tsc pipeline. These cases drive the walk directly against a probes
 * package on disk — no pnpm, no network — because what is under test is the
 * one property the text cannot show: that the walk NEVER contradicts the judge.
 * It names a field only where the compiler's own relation says the members do
 * not assign, and it stays silent on the shapes where per-member assignability
 * and whole-type assignability legitimately diverge.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { buildProbe } from '../src/capture/check-probe.js';
import { openProbeProgram } from '../src/capture/check-deep.js';
import { pairFieldReports, MAX_NAMED_FIELDS } from '../src/capture/check-fields.js';
import type { CheckPairSpec } from '../src/capture/api.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: {
    strict: true,
    skipLibCheck: true,
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

const KEYS = [
  'named',
  'agree',
  'rootunion',
  'indexed',
  'arrays',
  'tuples',
  'memberunion',
  'many',
] as const;

const PRODUCER = `export type named_Producer = { id: string; label: string; };
export type agree_Producer = { id: "live"; count: number; };
export type rootunion_Producer = { kind: "a"; id: string } | { kind: "b"; id: number };
export type indexed_Producer = { userName: string };
export type arrays_Producer = { rows: string[] };
export type tuples_Producer = { pair: [string, number] };
export type memberunion_Producer = { kind: "a" | "b" };
export type many_Producer = { a: string; b: string; c: string; d: string; e: string; f: string; g: string; h: string; i: string; j: string; };
`;

const CONSUMER = `export type named_Consumer = { id: number; label: string; };
export type agree_Consumer = { id: string; count: number; };
export type rootunion_Consumer = { kind: "a"; id: string };
export type indexed_Consumer = { username: string; [key: string]: string };
export type arrays_Consumer = { rows: number[] };
export type tuples_Consumer = { pair: [number, number] };
export type memberunion_Consumer = { kind: "a" };
export type many_Consumer = { a: number; b: number; c: number; d: number; e: number; f: number; g: number; h: number; i: number; j: number; };
`;

function spec(pairKey: string): CheckPairSpec {
  return {
    pair_key: pairKey,
    protocol: 'http',
    type_kind: 'response',
    producer: { service_name: 'producer', alias: `${pairKey}_Producer` },
    consumer: { service_name: 'consumer', alias: `${pairKey}_Consumer` },
  };
}

describe('the field report never contradicts the judge', () => {
  let dir: string;

  before(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1118-fields-'));
    fs.mkdirSync(path.join(dir, 'probes'), { recursive: true });
    fs.mkdirSync(path.join(dir, 'surfaces'), { recursive: true });
    fs.writeFileSync(path.join(dir, 'tsconfig.json'), TSCONFIG);
    fs.writeFileSync(path.join(dir, 'surfaces', 'producer.d.ts'), PRODUCER);
    fs.writeFileSync(path.join(dir, 'surfaces', 'consumer.d.ts'), CONSUMER);
    for (const key of KEYS) {
      const plan = buildProbe(spec(key), (s) => `@carrick/${s}`);
      fs.writeFileSync(path.join(dir, 'probes', plan.fileName), plan.source);
    }
  });

  after(() => {
    fs.rmSync(dir, { recursive: true, force: true });
  });

  function reportFor(key: (typeof KEYS)[number]) {
    const plan = buildProbe(spec(key), (s) => `@carrick/${s}`);
    return pairFieldReports(openProbeProgram(dir, [plan]), [plan]).get(plan.pairId);
  }

  it('names the member the compiler cannot assign, and only that one', () => {
    const report = reportFor('named');
    assert.ok(report, 'the walk must run and resolve both aliases');
    assert.deepStrictEqual(
      report.differences.map((d) => [d.path, d.nature]),
      [['id', 'type_differs']],
      // `label` assigns, so naming it would be a claim the judge never made.
      JSON.stringify(report.differences)
    );
    assert.strictEqual(report.differences[0].sentText, 'string');
    assert.strictEqual(report.differences[0].expectedText, 'number');
  });

  it('says nothing about members that assign, however differently they print', () => {
    // `"live"` and `string` print differently and assign fine.
    assert.deepStrictEqual(reportFor('agree')!.differences, []);
  });

  it('refuses a union root rather than guessing which member was meant', () => {
    assert.deepStrictEqual(reportFor('rootunion')!.differences, []);
  });

  it('refuses a receiver with an index signature, which accepts what it does not name', () => {
    // The consumer names `username` and accepts every other string-valued key,
    // so "the producer declares no such field" would be false of `userName`.
    assert.deepStrictEqual(reportFor('indexed')!.differences, []);
  });

  it('reports an array at the field that holds it, not at its library members', () => {
    assert.deepStrictEqual(
      reportFor('arrays')!.differences.map((d) => [d.path, d.nature]),
      [['rows', 'type_differs']]
    );
  });

  it('reports a tuple at the field that holds it, not at its positions', () => {
    assert.deepStrictEqual(
      reportFor('tuples')!.differences.map((d) => [d.path, d.nature]),
      [['pair', 'type_differs']]
    );
  });

  it('still names a MEMBER whose own type is a union', () => {
    const report = reportFor('memberunion');
    assert.deepStrictEqual(
      report!.differences.map((d) => d.path),
      ['kind'],
      'a union under a named member is exactly what a reader needs told'
    );
  });

  it('caps the list and says how many it did not name', () => {
    const report = reportFor('many')!;
    assert.strictEqual(report.differences.length, MAX_NAMED_FIELDS);
    assert.strictEqual(report.truncated, 10 - MAX_NAMED_FIELDS);
    // Ordered, so the same two types always produce the same text.
    assert.deepStrictEqual(
      report.differences.map((d) => d.path),
      ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h']
    );
  });

  it('has no entry at all when it could not run', () => {
    const plan = buildProbe(spec('named'), (s) => `@carrick/${s}`);
    const nowhere = path.join(dir, 'nowhere');
    assert.strictEqual(
      pairFieldReports(openProbeProgram(nowhere, [plan]), [plan]).size,
      0,
      'absence of a report must be distinguishable from an empty one'
    );
  });
});
