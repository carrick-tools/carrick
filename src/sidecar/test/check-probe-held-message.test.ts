/**
 * The judge's message rules, exported for the held check (HM-1).
 *
 * (1) The probe text is pinned: `buildProbe(...).source` for ten fixed socket
 *     and pub/sub pairs equals the text captured before the rules were
 *     extracted.
 * (2) Agreement: over socket pairs the judge's `compatible` and every
 *     `unverifiable` gate are silent in the held form and its `incompatible`
 *     speaks, by the native compiler over both texts. Two one-way
 *     differences are intended and listed as `known_difference`.
 * (3) `write_shadow` carries the message rules.
 */
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as os from 'node:os';
import { spawnSync } from 'node:child_process';
import { describe, it, before, after } from 'node:test';
import { buildProbe, judgeTolerances } from '../src/capture/check-probe.js';
import { runCheck } from '../src/capture/index.js';
import { heldMessageWire } from '../src/held-shadow.js';
import type { CheckPairSpec, CheckStubInput, CheckVerdict } from '../src/capture/api.js';
import { MESSAGE_SPECS, packageOf } from './check-probe-held-message-specs.js';
import { nativeTsc, scratch } from './held-shadow-support.js';
import { SIDECAR_PATH } from './helpers.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const GOLDEN: Record<string, string> = JSON.parse(
  fs.readFileSync(path.resolve(here, '..', '..', 'test', 'fixtures', 'check-probe-held-message.golden.json'), 'utf8')
);

describe('the judge probe text is unchanged by the message-rule extraction', () => {
  for (const { spec, tolerates } of MESSAGE_SPECS) {
    it(`${spec.pair_key}: buildProbe source equals the captured text`, () => {
      const { source } = buildProbe(spec, packageOf, undefined, () => tolerates);
      assert.equal(source, GOLDEN[spec.pair_key]);
    });
  }

  it('the golden file holds exactly the fixed specs', () => {
    assert.deepEqual(Object.keys(GOLDEN).sort(), MESSAGE_SPECS.map((m) => m.spec.pair_key).sort());
  });
});

interface Case {
  name: string;
  /** The emitter's payload type. */
  sent: string;
  /** The listener's parameter type. */
  expected: string;
  /** The judge's bucket for the pair (no `absent_body_tolerated` mark). An `any` side is the judge's own bucket, `gate_caught_baked_any`: unverifiable too. */
  bucket: 'compatible' | 'incompatible' | 'unverifiable' | 'gate_caught_baked_any';
  /** The judge's gate, for an `unverifiable` pair. */
  gate?: string;
  /**
   * An intended one-way difference: the held form is silent where the judge
   * speaks. The test asserts held silent only (see `heldSilence`).
   */
  known_difference?: 'absent payload' | 'byte listener';
}

const UPDATE = '{ id: string; status: string }';

const CASES: Case[] = [
  { name: 'match', sent: UPDATE, expected: UPDATE, bucket: 'compatible' },
  { name: 'missing field', sent: '{ id: string }', expected: UPDATE, bucket: 'incompatible' },
  { name: 'extra field', sent: '{ id: string; status: string; note: string }', expected: UPDATE, bucket: 'compatible' },
  { name: 'wrong field type', sent: '{ id: string; status: number }', expected: UPDATE, bucket: 'incompatible' },
  // The wrapped span is the payload argument only; a trailing ack callback and the listener's second parameter are not in either type.
  { name: 'ack callback argument', sent: UPDATE, expected: UPDATE, bucket: 'compatible' },
  { name: 'void payload', sent: 'undefined', expected: UPDATE, bucket: 'unverifiable', gate: 'consumer:void' },
  { name: 'void payload (void)', sent: 'void', expected: UPDATE, bucket: 'unverifiable', gate: 'consumer:void' },
  { name: 'unknown listener', sent: UPDATE, expected: 'unknown', bucket: 'unverifiable', gate: 'producer:unknown' },
  { name: 'any listener', sent: UPDATE, expected: 'any', bucket: 'gate_caught_baked_any', gate: 'producer:any' },
  { name: 'void listener', sent: UPDATE, expected: 'void', bucket: 'unverifiable', gate: 'producer:void' },
  { name: 'unknown emit payload', sent: 'unknown', expected: UPDATE, bucket: 'unverifiable', gate: 'consumer:unknown' },
  { name: 'any emit payload', sent: 'any', expected: UPDATE, bucket: 'gate_caught_baked_any', gate: 'consumer:any' },
  { name: 'never emit payload', sent: 'never', expected: UPDATE, bucket: 'unverifiable', gate: 'consumer:never' },
  // The listener's `absent_body_tolerated` mark is not stored, so the held form reads the tolerant rule.
  { name: 'absent payload', sent: `${UPDATE} | undefined`, expected: '{}', bucket: 'incompatible', known_difference: 'absent payload' },
  // The laptop's `isTopOrBytes` guard places no check on a byte-typed listener; Uint8Array stands for Buffer.
  { name: 'byte listener', sent: UPDATE, expected: 'Uint8Array', bucket: 'incompatible', known_difference: 'byte listener' },
];

const slug = (name: string): string => name.replace(/\W+/g, '_');

function writeStub(dir: string, serviceName: string, surface: string): void {
  const stubDir = path.join(dir, serviceName);
  fs.mkdirSync(path.join(stubDir, 'types'), { recursive: true });
  fs.writeFileSync(
    path.join(stubDir, 'package.json'),
    JSON.stringify({ name: `@carrick/${serviceName}`, version: '0.0.0-carrick', private: true, types: './types/surface.d.ts' }) + '\n'
  );
  fs.writeFileSync(path.join(stubDir, 'types', 'surface.d.ts'), surface);
}

/** The held check of one payload: the engine's declarations, then the call a placed check would write. */
function heldSource(c: Case): string {
  return (
    [
      ...judgeTolerances('__carrick_').declarations,
      ...heldMessageWire(),
      `type E = ${c.expected};`,
      `declare function send<V>(v: V, c: (o: __carrick_MsgSent<V, E>) => E): V;`,
      `declare const v: ${c.sent};`,
      'send(v, (o) => o);',
    ].join('\n') + '\n'
  );
}

/** The native compiler's error lines for one standalone file (strict, no libs beyond es2022). */
function tscErrors(source: string): string[] {
  const dir = scratch('held-msg-');
  const file = path.join(dir, 'a.ts');
  fs.writeFileSync(file, source);
  const run = spawnSync(nativeTsc(), ['--noEmit', '--strict', '--target', 'es2022', '--lib', 'es2022', '--ignoreConfig', '--pretty', 'false', file], {
    cwd: dir,
    encoding: 'utf8',
  });
  return run.stdout.split('\n').filter((l) => /error TS\d+/.test(l));
}

describe('the held message form agrees with the judge over socket pairs', () => {
  let root: string;
  let verdicts: Map<string, CheckVerdict>;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-held-msg-agree-'));
    // The listener (producer) declares the parameter type; the emitter (consumer) the payload.
    writeStub(root, 'orders', CASES.map((c) => `export type L_${slug(c.name)} = ${c.expected};`).join('\n') + '\n');
    writeStub(root, 'till', CASES.map((c) => `export type S_${slug(c.name)} = ${c.sent};`).join('\n') + '\n');
    const pairs: CheckPairSpec[] = CASES.map((c) => ({
      pair_key: c.name,
      protocol: 'socket',
      type_kind: 'response',
      producer: { service_name: 'orders', alias: `L_${slug(c.name)}` },
      consumer: { service_name: 'till', alias: `S_${slug(c.name)}` },
    }));
    const stubs: CheckStubInput[] = [
      { service_name: 'orders', stub_dir: path.join(root, 'orders') },
      { service_name: 'till', stub_dir: path.join(root, 'till') },
    ];
    const result = await runCheck({ stubs, pairs });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    verdicts = new Map(result.verdicts.map((v) => [v.pair_key, v]));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  for (const c of CASES) {
    const held = c.known_difference !== undefined ? 'is silent (known_difference)' : c.bucket === 'incompatible' ? 'speaks' : 'is silent';
    it(`orders/order:update: ${c.name}: judge ${c.bucket}, held ${held}`, () => {
      const v = verdicts.get(c.name);
      assert.ok(v, `verdict for ${c.name}`);
      assert.strictEqual(v.bucket, c.bucket, JSON.stringify(v));
      if (c.gate !== undefined) assert.strictEqual(v.gate, c.gate, JSON.stringify(v));
      const errors = tscErrors(heldSource(c));
      if (c.known_difference === 'byte listener') {
        // The shadow's declarations alone would speak here: the silence is the laptop's `isTopOrBytes` guard, which places no check.
        assert.ok(errors.length > 0, 'the declarations alone speak; the laptop guard places no check');
      } else if (c.known_difference === 'absent payload') {
        assert.deepEqual(errors, [], 'the tolerant form is silent where the unmarked judge speaks');
      } else if (c.bucket === 'incompatible') {
        assert.ok(errors.length > 0, 'held should speak');
      } else {
        assert.deepEqual(errors, []);
      }
    });
  }
});

describe("write_shadow carries the judge's message rules", () => {
  const FIXTURE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..', 'test', 'fixtures', 'held-node');

  it('write_shadow carries the message rules', () => {
    const run = spawnSync('node', [SIDECAR_PATH], {
      cwd: os.tmpdir(),
      input:
        [
          { action: 'init', request_id: 'i', repo_root: FIXTURE, scan_root: FIXTURE },
          { action: 'write_shadow', request_id: 's', out_dir: scratch('held-msg-shadow-') },
          { action: 'shutdown', request_id: 'x' },
        ]
          .map((l) => JSON.stringify(l))
          .join('\n') + '\n',
      encoding: 'utf8',
      timeout: 120_000,
      maxBuffer: 64 * 1024 * 1024,
    });
    const answer = run.stdout
      .split('\n')
      .filter((l) => l.trim().startsWith('{'))
      .map((l) => JSON.parse(l) as Record<string, any>)
      .find((a) => a.request_id === 's');
    assert.ok(answer && answer.status === 'success', run.stdout + run.stderr);
    assert.deepEqual(answer.message, heldMessageWire());
    assert.ok(answer.message.some((l: string) => l.startsWith('type __carrick_MsgSent<S, E>')));
    assert.ok(answer.message.some((l: string) => l.startsWith('type __carrick_IsUnknown<T>')));
  });

  it('names only prefixed declarations', () => {
    assert.deepEqual(heldMessageWire().join('\n').match(/(?<!__carrick_)\b(IsAny|IsUnknown|IsNever|IsVoid|MsgSent)\b/g), null);
  });
});
