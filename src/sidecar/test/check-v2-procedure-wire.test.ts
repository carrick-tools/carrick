/**
 * v2 check — an RPC procedure's wire form is unknown until its router's
 * transformer is a known fact (carrick#4402).
 *
 * A procedure of an RPC router (tRPC and similar) travels in its router's
 * transformer's form. With no transformer that is JSON, so a `Date` reaches
 * the client as a string. With a transformer that revives dates (superjson)
 * the `Date` arrives as a `Date`, and a client typed `string` breaks. The
 * check cannot tell the two apart yet, so where the declared reading and the
 * JSON reading of a procedure's payload disagree, it abstains: `unverifiable`,
 * gate `<side>:wire_form`. Where both readings agree, the verdict stands.
 *
 * Every non-procedure probe is unchanged byte for byte (the #4275 snapshots in
 * check-v2-graphql-wire.test.ts).
 *
 * Real vendored pnpm + tsc, in the style of check-v2-unwidened.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub, runCheck } from '../src/capture/index.js';
import type { CaptureStubResult, CheckPairSpec, CheckVerdict } from '../src/capture/api.js';
import { buildProbe, unwidenedAliasOf, wireFormFor } from '../src/capture/check-probe.js';

function captureLiterals(
  root: string,
  serviceName: string,
  literals: Record<string, string>
): CaptureStubResult {
  const repoRoot = path.join(root, `${serviceName}-repo`);
  fs.mkdirSync(repoRoot, { recursive: true });
  const result = captureStub({
    repoRoot,
    serviceName,
    outDir: path.join(root, `${serviceName}-stub`),
    anchors: Object.entries(literals).map(([alias, text]) => ({
      kind: 'literal' as const,
      alias,
      type_text: text,
      anchor_origin: 'deterministic-infer' as const,
    })),
  });
  assert.strictEqual(result.success, true, JSON.stringify(result.errors));
  return result;
}

const WIRE_NOTE =
  "The producer's type is compared in the form JSON puts on the wire: a value with a toJSON() method (a Date, for example) travels as what it serialises to.";

const ROW = `{ id: string; at: Date }`;

describe('check_v2 abstains on a procedure whose two wire readings disagree (carrick#4402)', () => {
  let root: string;
  let verdicts: Map<string, CheckVerdict>;

  const pair = (
    pair_key: string,
    producerAlias: string,
    consumerAlias: string,
    over: Partial<CheckPairSpec> = {}
  ): CheckPairSpec => ({
    pair_key,
    protocol: 'http',
    type_kind: 'response',
    producer: { service_name: 'rpc-router', alias: producerAlias },
    consumer: { service_name: 'rpc-client', alias: consumerAlias },
    procedure: true,
    ...over,
  });

  const pairs: CheckPairSpec[] = [
    pair('a-date', 'P_Row', 'C_Date'),
    pair('b-string', 'P_Row', 'C_String'),
    pair('c-number', 'P_Row', 'C_Number'),
    pair('d-plain-route', 'P_Row', 'C_String', { procedure: undefined }),
    pair('e-request', 'P_Input', 'C_Input', { type_kind: 'request' }),
    pair('f-unwidened', 'P_Wide', 'C_String'),
  ];

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4402-check-'));
    const producer = captureLiterals(root, 'rpc-router', {
      P_Row: ROW,
      P_Input: `{ id: string; at: string }`,
      // Pair (b) with an unwidened reading shipped beside it: in JSON form
      // that reading fits a string client and would clear the pair.
      P_Wide: ROW,
      [unwidenedAliasOf('P_Wide')]: ROW,
    });
    const consumer = captureLiterals(root, 'rpc-client', {
      C_Date: `{ at: Date }`,
      C_String: `{ at: string }`,
      C_Number: `{ at: number }`,
      C_Input: ROW,
    });
    const result = await runCheck({
      stubs: [
        { service_name: 'rpc-router', stub_dir: producer.stub_dir },
        { service_name: 'rpc-client', stub_dir: consumer.stub_dir },
      ],
      pairs,
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    verdicts = new Map(result.verdicts.map((v) => [v.pair_key, v]));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('(a) a client that declares the Date agrees in both readings: compatible, no wire note', () => {
    const v = verdicts.get('a-date')!;
    assert.strictEqual(v.bucket, 'compatible', JSON.stringify(v));
    assert.ok(!(v.notes ?? []).includes(WIRE_NOTE), JSON.stringify(v.notes));
  });

  it('(b) a client reading the Date as a string: unverifiable, the transformer decides', () => {
    const v = verdicts.get('b-string')!;
    assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
    assert.strictEqual(v.gate, 'producer:wire_form');
    assert.strictEqual(v.resolved, false);
    assert.ok(v.diagnostic && v.diagnostic.length > 0, JSON.stringify(v));
    assert.strictEqual(v.unresolved_side, undefined, 'no side to retype');
  });

  it('(c) a client reading the Date as a number breaks in both readings: incompatible', () => {
    const v = verdicts.get('c-number')!;
    assert.strictEqual(v.bucket, 'incompatible', JSON.stringify(v));
  });

  it('(d) the same pair on a plain route is compared in JSON form, as before', () => {
    const v = verdicts.get('d-plain-route')!;
    assert.strictEqual(v.bucket, 'compatible', JSON.stringify(v));
    assert.ok((v.notes ?? []).includes(WIRE_NOTE), JSON.stringify(v.notes));
  });

  it('(e) a procedure input the client sends with a Date, read as a string: unverifiable', () => {
    const v = verdicts.get('e-request')!;
    assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
    assert.strictEqual(v.gate, 'consumer:wire_form');
  });

  it('(f) an unwidened reading never clears a procedure in JSON form', () => {
    const v = verdicts.get('f-unwidened')!;
    assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
    assert.strictEqual(v.gate, 'producer:wire_form');
  });
});

describe('the procedure probe (carrick#4402)', () => {
  const spec = (procedure: boolean): CheckPairSpec => ({
    pair_key: 'k',
    protocol: 'http',
    type_kind: 'response',
    producer: { service_name: 'api', alias: 'P' },
    consumer: { service_name: 'web', alias: 'C' },
    ...(procedure ? { procedure: true as const } : {}),
  });

  it('wireFormFor answers unknown for either sending side of a procedure', () => {
    assert.strictEqual(wireFormFor('http', 'response', 'producer', { procedure: true }), 'unknown');
    assert.strictEqual(wireFormFor('http', 'request', 'consumer', { procedure: true }), 'unknown');
    // The receiving side reads what it reads as declared.
    assert.strictEqual(wireFormFor('http', 'response', 'consumer', { procedure: true }), 'declared');
    assert.strictEqual(wireFormFor('http', 'request', 'producer', { procedure: true }), 'declared');
    // No fact: JSON, as #4275 T1.
    assert.strictEqual(wireFormFor('http', 'response', 'producer'), 'json');
  });

  it('keeps the declared line decisive, holds the gate, and no JSON-form reading', () => {
    const plan = buildProbe(spec(true), (s) => `@carrick/${s}`, () => 'P_Unwidened');
    assert.strictEqual(plan.wireAssignmentLine, undefined);
    assert.strictEqual(plan.unwidenedFromLine, undefined);
    assert.doesNotMatch(plan.source, /sentWire/, plan.source);
    assert.ok([...plan.gateLines.values()].includes('sent:wire_form'));
  });

  it('a plain route probe holds no wire_form gate', () => {
    const plan = buildProbe(spec(false), (s) => `@carrick/${s}`);
    assert.ok(![...plan.gateLines.values()].includes('sent:wire_form'));
    assert.match(plan.source, /sentWire/);
  });
});
