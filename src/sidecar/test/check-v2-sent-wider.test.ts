/**
 * carrick#4685: a producer field typed by a database or JSON column stays
 * incompatible, and its row says why.
 *
 * A bare `string` where the consumer names literals of it, or a generic JSON
 * value where the consumer names an object shape, is wider than what the
 * consumer reads. The verdict is the break it always was; the field notes
 * name the field and say the sender's type is open, so the reader can tighten
 * it. Every other difference gets no such note.
 *
 * The pairs are judged by the compiler itself: each probe is checked in one
 * program and its real diagnostics go through the classifier with the field
 * report the same program walks. No pnpm, no network.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import ts from 'typescript';
import { buildProbe, decisiveAssignmentLine, type ProbePlan } from '../src/capture/check-probe.js';
import { openProbeProgram } from '../src/capture/check-deep.js';
import { pairFieldReports } from '../src/capture/check-fields.js';
import { classifyPair, type RawDiagnostic } from '../src/capture/check-classify.js';
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

// Two spellings of the generic JSON value a column is typed with: a plain
// recursive alias, and the interface-and-mapped-type form.
const PRODUCER = `type Doc = string | number | boolean | null | Doc[] | { [key: string]: Doc };
interface DocList extends Array<Cell> {}
type DocMap = { [K in string]?: Cell };
type Cell = string | number | boolean | DocMap | DocList | null;
export type openstring_Producer = { status: string; id: string };
export type opennumber_Producer = { tier: number; id: string };
export type openjson_Producer = { meta: Doc; id: string };
export type openjsonmapped_Producer = { meta: Cell; createdAt: Date };
export type both_Producer = { status: string; meta: Cell; createdAt: Date };
export type gqlopen_Producer = { status: string; id: string };
export type addednull_Producer = { status: string | null };
export type addedmember_Producer = { kind: 'a' | 'b' | 'c' };
export type missingtop_Producer = { id: string };
export type missingarray_Producer = { rows: { id: string }[] };
export type missingnullable_Producer = { owner: { id: string } | null };
export type scalar_Producer = { count: number };
export type openbeside_Producer = { status: string; count: number };
export type openhiddennull_Producer = { status: string; owner: { id: string } | null };
export type openweak_Producer = { status: string; opts: { a: string } };
export type openmissing_Producer = { status: string };
export type openarray_Producer = { tags: string[] };
export type rootarray_Producer = { id: string }[];
export type jsonarray_Producer = { items: Doc };
export type reqopen_Producer = { status: 'active' | 'closed' };
export type deepopen_Producer = { a: { b: { c: { d: { status: string } } } } };
export type notjson_Producer = { meta: string | number | boolean | null };
`;

const CONSUMER = `export type openstring_Consumer = { status: 'active' | 'closed'; id: string };
export type opennumber_Consumer = { tier: 1 | 2 | 3; id: string };
export type openjson_Consumer = { meta: { color: string; size: number }; id: string };
export type openjsonmapped_Consumer = { meta: { color: string }; createdAt: string };
export type both_Consumer = { status: 'active' | 'closed'; meta: { color: string }; createdAt: string };
export type gqlopen_Consumer = { status: 'active' | 'closed'; id: string };
export type addednull_Consumer = { status: string };
export type addedmember_Consumer = { kind: 'a' | 'b' };
export type missingtop_Consumer = { id: string; createdAt: string };
export type missingarray_Consumer = { rows: { id: string; createdAt: string }[] };
export type missingnullable_Consumer = { owner: { id: string; createdAt: string } | null };
export type scalar_Consumer = { count: string };
export type openbeside_Consumer = { status: 'active' | 'closed'; count: string };
export type openhiddennull_Consumer = { status: 'active' | 'closed'; owner: { id: string } };
export type openweak_Consumer = { status: 'active' | 'closed'; opts: { b?: string } };
export type openmissing_Consumer = { status: 'active' | 'closed'; createdAt: string };
export type openarray_Consumer = { tags: ('a' | 'b')[] };
export type rootarray_Consumer = { id: string; createdAt: string }[];
export type jsonarray_Consumer = { items: { id: string }[] };
export type reqopen_Consumer = { status: string };
export type deepopen_Consumer = { a: { b: { c: { d: { status: 'active' | 'closed' } } } } };
export type notjson_Consumer = { meta: { color: string } };
`;

const KEYS = [
  'openstring',
  'opennumber',
  'openjson',
  'openjsonmapped',
  'both',
  'gqlopen',
  'addednull',
  'addedmember',
  'missingtop',
  'missingarray',
  'missingnullable',
  'scalar',
  'openbeside',
  'openhiddennull',
  'openweak',
  'openmissing',
  'openarray',
  'rootarray',
  'jsonarray',
  'notjson',
  'reqopen',
  'deepopen',
] as const;
type Key = (typeof KEYS)[number];

function spec(pairKey: Key): CheckPairSpec {
  return {
    pair_key: pairKey,
    protocol: pairKey.startsWith('gql') ? 'graphql' : 'http',
    type_kind: pairKey.startsWith('req') ? 'request' : 'response',
    producer: { service_name: 'producer', alias: `${pairKey}_Producer` },
    consumer: { service_name: 'consumer', alias: `${pairKey}_Consumer` },
  };
}

const OPEN_NOTE = /types it as any (string|number|boolean|JSON value)/;

describe('a producer wider than the consumer reads stays incompatible and says why (carrick#4685)', () => {
  let dir: string;
  const plans = new Map<Key, ProbePlan>();

  before(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4685-wider-'));
    fs.mkdirSync(path.join(dir, 'probes'), { recursive: true });
    fs.mkdirSync(path.join(dir, 'surfaces'), { recursive: true });
    fs.writeFileSync(path.join(dir, 'tsconfig.json'), TSCONFIG);
    fs.writeFileSync(path.join(dir, 'surfaces', 'producer.d.ts'), PRODUCER);
    fs.writeFileSync(path.join(dir, 'surfaces', 'consumer.d.ts'), CONSUMER);
    for (const key of KEYS) {
      const plan = buildProbe(spec(key), (s) => `@carrick/${s}`);
      plans.set(key, plan);
      fs.writeFileSync(path.join(dir, 'probes', plan.fileName), plan.source);
    }
  });

  after(() => {
    fs.rmSync(dir, { recursive: true, force: true });
  });

  /** The verdict the classifier reaches on the compiler's own diagnostics. */
  function judge(key: Key) {
    const plan = plans.get(key)!;
    const opened = openProbeProgram(dir, [plan]);
    assert.ok(opened, 'the probe program must open');
    const source = opened.program
      .getSourceFiles()
      .find((sf) => sf.fileName.endsWith(`/probes/${plan.fileName}`));
    assert.ok(source, 'the probe must be in the program');
    const probeDiags: RawDiagnostic[] = opened.program
      .getSemanticDiagnostics(source)
      .map((d) => ({
        file: `probes/${plan.fileName}`,
        line: source.getLineAndCharacterOfPosition(d.start ?? 0).line + 1,
        col: source.getLineAndCharacterOfPosition(d.start ?? 0).character + 1,
        code: d.code,
        message: ts.flattenDiagnosticMessageText(d.messageText, '\n'),
      }));
    assert.ok(
      probeDiags.some((d) => d.line === decisiveAssignmentLine(plan)),
      `${key}: the compiler must reject the decisive assignment: ${JSON.stringify(probeDiags)}`
    );
    const report = pairFieldReports(opened, [plan]).get(plan.pairId);
    const verdict = classifyPair({
      plan,
      probeDiags,
      poisonReason: () => undefined,
      scrubCtx: { workspaceRoot: dir, packageLabelOf: () => undefined },
      deepFindings: { sent: [], expected: [] },
      fieldReport: report,
    });
    return { verdict, report };
  }

  /** Incompatible, no gate; returns the wording the row carries. */
  function incompatible(key: Key) {
    const { verdict, report } = judge(key);
    assert.strictEqual(verdict.bucket, 'incompatible', `${key}: ${JSON.stringify({ verdict, report })}`);
    assert.strictEqual(verdict.gate, undefined);
    return { notes: verdict.notes ?? [], report: report! };
  }

  function assertOpenNote(key: Key, expected: string[]) {
    const { notes } = incompatible(key);
    assert.deepStrictEqual(
      notes.filter((n) => OPEN_NOTE.test(n)),
      expected
    );
  }

  function assertNoOpenNote(key: Key) {
    const { notes, report } = incompatible(key);
    assert.deepStrictEqual(
      notes.filter((n) => OPEN_NOTE.test(n)),
      []
    );
    assert.ok(
      report.differences.every((d) => d.openLeaf === undefined),
      `${key}: no leaf is open`
    );
  }

  describe('open leaves are worded', () => {
    it('a bare string where the consumer names string literals', () => {
      assertOpenNote('openstring', [
        "'status': the producer types it as any string, the consumer only as specific literal values",
      ]);
      assert.deepStrictEqual(
        judge('openstring').report!.differences.map((d) => [d.path, d.nature, d.openLeaf]),
        [['status', 'type_differs', 'string']]
      );
    });

    it('a bare number where the consumer names number literals', () => {
      assertOpenNote('opennumber', [
        "'tier': the producer types it as any number, the consumer only as specific literal values",
      ]);
    });

    it('a recursive JSON value where the consumer names an object shape', () => {
      assertOpenNote('openjson', [
        "'meta': the producer types it as any JSON value, the consumer as a specific object shape",
      ]);
    });

    it('the interface-and-mapped spelling of the JSON value, beside a Date that travels as a string', () => {
      assertOpenNote('openjsonmapped', [
        "'meta': the producer types it as any JSON value, the consumer as a specific object shape",
      ]);
      assert.strictEqual(judge('openjsonmapped').report!.wireApplied, true);
    });

    it('an open string and an open JSON value in one pair', () => {
      assertOpenNote('both', [
        "'meta': the producer types it as any JSON value, the consumer as a specific object shape",
        "'status': the producer types it as any string, the consumer only as specific literal values",
      ]);
    });

    it('a GraphQL response compared through the same field report', () => {
      assertOpenNote('gqlopen', [
        "'status': the producer types it as any string, the consumer only as specific literal values",
      ]);
    });

    it('a request body uses the request side names', () => {
      assertOpenNote('reqopen', [
        "'status': the consumer types it as any string, the producer only as specific literal values",
      ]);
    });

    it('an open leaf beside a real mismatch words only the open leaf', () => {
      assertOpenNote('openbeside', [
        "'status': the producer types it as any string, the consumer only as specific literal values",
      ]);
    });

    it('an open leaf beside a missing key', () => {
      assertOpenNote('openmissing', [
        "'status': the producer types it as any string, the consumer only as specific literal values",
      ]);
    });
  });

  describe('closed differences carry no such wording', () => {
    for (const [key, title] of [
      ['addednull', 'an added null (string | null where string is read)'],
      ['addedmember', "an added union member ('a' | 'b' | 'c' where 'a' | 'b' is read)"],
      ['missingtop', 'a missing key at the top level'],
      ['missingarray', 'a missing key under an array element'],
      ['missingnullable', 'a missing key under a nullable object'],
      ['scalar', 'a mismatched scalar (number where string is read)'],
      ['openarray', 'an array of strings where an array of literals is read'],
      ['rootarray', 'a missing key under a root array'],
      ['jsonarray', 'a JSON value where an array of shapes is read'],
      ['notjson', 'a union of JSON scalars that is not the recursive JSON value'],
    ] as const) {
      it(title, () => assertNoOpenNote(key));
    }
  });
});
