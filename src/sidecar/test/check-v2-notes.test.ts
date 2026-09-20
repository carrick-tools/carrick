/**
 * carrick#1341: the channel for a statement that is not a mismatch.
 *
 * Two things a compat check can now find are true of a pair it called
 * COMPATIBLE, so neither can ride on `diagnostic`, which exists only beside a
 * mismatch: an optionality gap (the sender always provides a field the
 * receiver declares optional — legal, assigns, and still a drift between two
 * sources), and the note that the comparison was made against the JSON wire
 * form rather than the declared one (carrick#1340), which is why a producer
 * `Date` read as a `string` is not reported.
 *
 * Two halves are under test and they fail for different reasons:
 *   - the walk really finds both statements on pairs the compiler's own
 *     relation calls compatible (driven against a probes package on disk, no
 *     pnpm, no network, same harness as check-v2-field-report.test.ts);
 *   - `classifyPair` puts them on `notes` for EVERY bucket, and a note never
 *     moves `bucket`, `resolved` or `unresolved_reason`.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { buildProbe } from '../src/capture/check-probe.js';
import { openProbeProgram } from '../src/capture/check-deep.js';
import { pairFieldReports, fieldReportNotes } from '../src/capture/check-fields.js';
import { classifyPair } from '../src/capture/check-classify.js';
import type { CheckPairSpec } from '../src/capture/api.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: {
    strict: true,
    skipLibCheck: true,
    noEmit: true,
    module: 'esnext',
    moduleResolution: 'bundler',
    target: 'es2022',
    lib: ['es2022'],
    baseUrl: '.',
    paths: {
      '@carrick/producer': ['./surfaces/producer.d.ts'],
      '@carrick/consumer': ['./surfaces/consumer.d.ts'],
    },
  },
  include: ['probes', 'surfaces'],
});

// `gap`: the producer ALWAYS sends `note`; the consumer declares it optional.
//        That assigns, so the pair is compatible and no diagnostic can exist.
// `wire`: the producer returns a `Date` the consumer reads as a `string`.
//        Serialisation makes them the same bytes (carrick#1340).
// `quiet`: two shapes that agree with nothing to observe at all.
const KEYS = ['gap', 'wire', 'quiet'] as const;

const PRODUCER = `export type gap_Producer = { id: string; note: string };
export type wire_Producer = { id: string; at: Date };
export type quiet_Producer = { id: string };
`;

const CONSUMER = `export type gap_Consumer = { id: string; note?: string };
export type wire_Consumer = { id: string; at: string };
export type quiet_Consumer = { id: string };
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

describe('a compatible pair can still have something to say (carrick#1341)', () => {
  let dir: string;

  before(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1341-notes-'));
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

  function notesFor(key: (typeof KEYS)[number]): string[] {
    const plan = buildProbe(spec(key), (s) => `@carrick/${s}`);
    const report = pairFieldReports(openProbeProgram(dir, [plan]), [plan]).get(plan.pairId);
    assert.ok(report, 'the walk must run and resolve both aliases');
    return fieldReportNotes(report, plan.direction.sent, plan.direction.expected);
  }

  it('states an optionality gap on a pair that assigns', () => {
    const notes = notesFor('gap');
    assert.deepStrictEqual(notes, [
      "'note' is always sent by the producer and optional on the consumer.",
    ]);
  });

  it('states that the comparison was made against the serialised form', () => {
    const notes = notesFor('wire');
    assert.strictEqual(notes.length, 1, JSON.stringify(notes));
    assert.match(notes[0], /compared in the form JSON puts on the wire/);
    assert.match(notes[0], /^The producer's type/, 'names the SENDING side');
  });

  it('says nothing about a pair with nothing to observe', () => {
    assert.deepStrictEqual(notesFor('quiet'), []);
  });
});

describe('notes reach every bucket, and move no verdict (carrick#1341)', () => {
  const plan = buildProbe(
    {
      pair_key: 'p~c',
      protocol: 'http',
      type_kind: 'response',
      producer: { service_name: 'producer', alias: 'P' },
      consumer: { service_name: 'consumer', alias: 'C' },
    },
    (s) => `@carrick/${s}`
  );
  const scrubCtx = { workspaceRoot: '/tmp/ws', packageLabelOf: () => undefined };
  const noPoison = () => undefined;
  const clean = { sent: [], expected: [] };

  // The one statement the whole ticket is about: a report on a pair with NO
  // diagnostics at all, which classifies compatible.
  const gapReport = {
    differences: [{ path: 'note', nature: 'optional_in_expected' as const }],
    truncated: 0,
    wireApplied: true,
  };

  it('a compatible verdict carries its notes', () => {
    const v = classifyPair({
      plan,
      probeDiags: [],
      poisonReason: noPoison,
      scrubCtx,
      deepFindings: clean,
      fieldReport: gapReport,
    });
    assert.strictEqual(v.bucket, 'compatible', 'the note must not move the bucket');
    assert.strictEqual(v.resolved, true, 'the note must not move fact-ness');
    assert.strictEqual(v.diagnostic, undefined, 'a note is not a diagnostic');
    assert.strictEqual(v.notes.length, 2);
    assert.match(v.notes[0], /compared in the form JSON puts on the wire/);
    assert.strictEqual(
      v.notes[1],
      "'note' is always sent by the producer and optional on the consumer."
    );
  });

  it('a gated verdict that compared nothing carries no notes', () => {
    const anyLine = [...plan.gateLines].find(([, n]) => n === 'sent:any')![0];
    const v = classifyPair({
      plan,
      probeDiags: [
        {
          file: `packages/carrick-probes/probes/${plan.fileName}`,
          line: anyLine,
          col: 1,
          code: 2344,
          message: 'x',
        },
      ],
      poisonReason: noPoison,
      scrubCtx,
      deepFindings: clean,
    });
    assert.strictEqual(v.bucket, 'gate_caught_baked_any');
    assert.deepStrictEqual(v.notes, [], 'no comparison happened, so nothing to observe');
  });

  it('a pair with no field report at all states no notes, which is not a claim', () => {
    const v = classifyPair({
      plan,
      probeDiags: [],
      poisonReason: noPoison,
      scrubCtx,
      deepFindings: clean,
    });
    assert.deepStrictEqual(v.notes, []);
  });
});
