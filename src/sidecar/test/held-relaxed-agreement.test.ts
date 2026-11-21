/**
 * Incompatible-rule T1, property 5: the held form the early check writes still
 * agrees with the judge after the relaxed re-check lands.
 *
 * The held check takes its rule text from the judge's exports (`heldWire`,
 * `heldMessageWire`, `judgeTolerances`). The re-check adds evidence beside the
 * judge's verdict and no rule: over the T1 seeds, every pair the judge calls
 * incompatible (not proven or proven) the held form speaks on, every pair it
 * calls compatible the held form is silent on, and no relaxed helper reaches
 * the held rules.
 *
 * Known gap, by design of T1: the held form speaks on a pair whose evidence
 * reads not proven. Once the scanner's relabel (T3) demotes such a pair, the
 * held check still speaks where the index does not.
 */
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { spawnSync } from 'node:child_process';
import { describe, it, before, after } from 'node:test';
import { judgeTolerances } from '../src/capture/check-probe.js';
import { runCheck } from '../src/capture/index.js';
import { heldMessageWire, heldWire } from '../src/held-shadow.js';
import type { CheckPairSpec, CheckStubInput, CheckVerdict } from '../src/capture/api.js';
import { nativeTsc, scratch } from './held-shadow-support.js';

interface Case {
  name: string;
  protocol: 'http' | 'socket';
  type_kind: 'response' | 'request' | 'both';
  /** The sending side's type. */
  sent: string;
  /** The receiving side's type. */
  expected: string;
  bucket: 'compatible' | 'incompatible';
  /** The relaxed re-check's reading, for an incompatible pair. */
  proven?: boolean;
}

const CASES: Case[] = [
  { name: 'agrees', protocol: 'http', type_kind: 'response', sent: '{ id: string }', expected: '{ id: string }', bucket: 'compatible' },
  { name: 'date on the wire', protocol: 'http', type_kind: 'response', sent: '{ at: Date }', expected: '{ at: string }', bucket: 'compatible' },
  { name: 'open kind', protocol: 'http', type_kind: 'response', sent: '{ kind: string }', expected: "{ kind: 'card' | 'wire' }", bucket: 'incompatible', proven: false },
  { name: 'null where required', protocol: 'http', type_kind: 'response', sent: '{ createdOn: string | null }', expected: '{ createdOn: string }', bucket: 'incompatible', proven: false },
  { name: 'null where optional', protocol: 'http', type_kind: 'request', sent: '{ logo: string | null }', expected: '{ logo?: string }', bucket: 'incompatible', proven: false },
  { name: 'weak cast', protocol: 'http', type_kind: 'response', sent: '{ order: { id: string } }', expected: '{ error?: string }', bucket: 'incompatible', proven: false },
  { name: 'missing field', protocol: 'http', type_kind: 'response', sent: '{ id: string }', expected: '{ id: string; openedAt: string }', bucket: 'incompatible', proven: true },
  { name: 'wrong kind', protocol: 'http', type_kind: 'response', sent: '{ count: string }', expected: '{ count: number }', bucket: 'incompatible', proven: true },
  { name: 'socket null', protocol: 'socket', type_kind: 'both', sent: '{ room: string | null }', expected: '{ room?: string }', bucket: 'incompatible', proven: false },
  { name: 'socket missing', protocol: 'socket', type_kind: 'both', sent: '{ id: string }', expected: '{ id: string; room: string }', bucket: 'incompatible', proven: true },
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

/** The held check of one pair: the engine's declarations, then the call a placed check writes. */
function heldSource(c: Case): string {
  const send =
    c.protocol === 'socket'
      ? [
          ...heldMessageWire(),
          `type E = ${c.expected};`,
          `declare function send<V>(v: V, c: (o: __carrick_MsgSent<V, E>) => E): V;`,
        ]
      : [
          ...heldWire(),
          ...judgeTolerances('__carrick_').declarations,
          `type E = ${c.expected};`,
          `declare function send<V>(v: V, c: (o: __carrick_Wire<V, E>) => E): V;`,
        ];
  return [...send, `declare const v: ${c.sent};`, 'send(v, (o) => o);'].join('\n') + '\n';
}

function tscErrors(source: string): string[] {
  const dir = scratch('held-relaxed-');
  const file = path.join(dir, 'a.ts');
  fs.writeFileSync(file, source);
  const run = spawnSync(nativeTsc(), ['--noEmit', '--strict', '--target', 'es2022', '--lib', 'es2022', '--ignoreConfig', '--pretty', 'false', file], {
    cwd: dir,
    encoding: 'utf8',
  });
  return run.stdout.split('\n').filter((l) => /error TS\d+/.test(l));
}

describe('the held form agrees with the judge beside the relaxed re-check (incompatible-rule T1)', () => {
  let root: string;
  let verdicts: Map<string, CheckVerdict>;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-held-relaxed-'));
    // The sending side is the producer for an HTTP response, the consumer otherwise.
    const sentIsProducer = (c: Case) => c.protocol === 'http' && c.type_kind === 'response';
    writeStub(root, 'api', CASES.map((c) => `export type P_${slug(c.name)} = ${sentIsProducer(c) ? c.sent : c.expected};`).join('\n') + '\n');
    writeStub(root, 'web', CASES.map((c) => `export type C_${slug(c.name)} = ${sentIsProducer(c) ? c.expected : c.sent};`).join('\n') + '\n');
    const pairs: CheckPairSpec[] = CASES.map((c) => ({
      pair_key: c.name,
      protocol: c.protocol,
      type_kind: c.type_kind,
      producer: { service_name: 'api', alias: `P_${slug(c.name)}` },
      consumer: { service_name: 'web', alias: `C_${slug(c.name)}` },
    }));
    const stubs: CheckStubInput[] = [
      { service_name: 'api', stub_dir: path.join(root, 'api') },
      { service_name: 'web', stub_dir: path.join(root, 'web') },
    ];
    const result = await runCheck({ stubs, pairs });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    verdicts = new Map(result.verdicts.map((v) => [v.pair_key, v]));
  });

  after(() => fs.rmSync(root, { recursive: true, force: true }));

  for (const c of CASES) {
    it(`${c.protocol} ${c.type_kind}: ${c.name}: judge ${c.bucket}, held ${c.bucket === 'incompatible' ? 'speaks' : 'is silent'}`, () => {
      const v = verdicts.get(c.name)!;
      assert.strictEqual(v.bucket, c.bucket, JSON.stringify(v));
      assert.strictEqual(v.break_evidence?.proven, c.proven, JSON.stringify(v.break_evidence));
      const errors = tscErrors(heldSource(c));
      if (c.bucket === 'incompatible') assert.ok(errors.length > 0, 'held should speak');
      else assert.deepEqual(errors, []);
    });
  }

  it('no relaxed helper reaches the held rules', () => {
    const held = [...heldWire(), ...heldMessageWire(), ...judgeTolerances('__carrick_').declarations].join('\n');
    assert.equal(/__rx_/.test(held), false);
  });
});
