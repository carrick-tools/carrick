/**
 * Incompatible-rule T1, end to end through `runCheck` on the judge's own
 * compiler (TypeScript 7's native tsc where installed).
 *
 * The relaxed re-check is evidence only:
 *   (2) it runs for the pairs the judge called incompatible and no others;
 *   (3) a re-check that throws, ends abnormally or runs out of time leaves
 *       every verdict as the judge made it, with `proven: true` and the cause;
 *   (4) it never changes a bucket, a code, a diagnostic, a note or `resolved`:
 *       the verdicts with a failed re-check equal those with a working one in
 *       everything but `break_evidence`.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runCheck } from '../src/capture/index.js';
import { RELAXED_DIR } from '../src/capture/check.js';
import { REMAINS } from '../src/capture/check-relaxed.js';
import type { CheckPairSpec, CheckResult, CheckStubInput, CheckVerdict } from '../src/capture/api.js';

let root: string;
let stubs: CheckStubInput[];

function writeStub(name: string, surface: string): void {
  const dir = path.join(root, name);
  fs.mkdirSync(path.join(dir, 'types'), { recursive: true });
  fs.writeFileSync(
    path.join(dir, 'package.json'),
    JSON.stringify({ name: `@carrick/${name}`, version: '0.0.0-carrick', private: true, types: './types/surface.d.ts' })
  );
  fs.writeFileSync(path.join(dir, 'types', 'surface.d.ts'), surface);
}

const SEEDS: Array<{ key: string; protocol: CheckPairSpec['protocol']; type_kind: CheckPairSpec['type_kind']; producer: string; consumer: string }> = [
  { key: 'agrees', protocol: 'http', type_kind: 'response', producer: `{ id: string }`, consumer: `{ id: string }` },
  { key: 'anyside', protocol: 'http', type_kind: 'response', producer: `any`, consumer: `{ id: string }` },
  { key: 'openkind', protocol: 'http', type_kind: 'response', producer: `{ kind: string }`, consumer: `{ kind: 'card' | 'wire' }` },
  { key: 'nullrequired', protocol: 'http', type_kind: 'response', producer: `{ createdOn: string | null }[]`, consumer: `{ createdOn: string }[]` },
  { key: 'weakcast', protocol: 'http', type_kind: 'response', producer: `{ order: { id: string } }`, consumer: `{ error?: string }` },
  { key: 'missingfield', protocol: 'http', type_kind: 'response', producer: `{ id: string }`, consumer: `{ id: string; openedAt: string }` },
  { key: 'socketnull', protocol: 'socket', type_kind: 'both', producer: `{ room?: string }`, consumer: `{ room: string | null }` },
  { key: 'graphqlleaf', protocol: 'graphql', type_kind: 'response', producer: `{ status: string }`, consumer: `{ status: 'ACTIVE' | 'PAUSED' }` },
  // The compiler stops comparing two rebuilt types nested three deep on each
  // side and assumes them related, so the helpers rebuild an array as an
  // object of its elements: under three levels of arrays an open value is
  // still allowed, and a missing field or another kind still proven.
  { key: 'openunderdeeparrays', protocol: 'http', type_kind: 'response', producer: `{ x: { a: { b: { c: { kind: string }[] }[] }[] } }`, consumer: `{ x: { a: { b: { c: { kind: 'card' | 'wire' }[] }[] }[] } }` },
  { key: 'missingunderdeeparrays', protocol: 'http', type_kind: 'response', producer: `{ kind: string; x: { a: { b: { c: { id: string }[] }[] }[] } }`, consumer: `{ kind: 'card' | 'wire'; x: { a: { b: { c: { id: string; openedAt: string }[] }[] }[] } }` },
  { key: 'dateunderdeeparrays', protocol: 'http', type_kind: 'response', producer: `{ a: { b: { c: { at: string }[] }[] }[] }`, consumer: `{ a: { b: { c: { at: Date }[] }[] }[] }` },
];

const PAIRS: CheckPairSpec[] = SEEDS.map((s) => ({
  pair_key: s.key,
  protocol: s.protocol,
  type_kind: s.type_kind,
  producer: { service_name: 'orders', alias: `${s.key}_P` },
  consumer: { service_name: 'web', alias: `${s.key}_C` },
}));

const INCOMPATIBLE = ['openkind', 'nullrequired', 'weakcast', 'missingfield', 'socketnull', 'graphqlleaf', 'openunderdeeparrays', 'missingunderdeeparrays', 'dateunderdeeparrays'];

function byKey(result: CheckResult): Map<string, CheckVerdict> {
  return new Map(result.verdicts.map((v) => [v.pair_key, v]));
}

function withoutEvidence(result: CheckResult): Omit<CheckVerdict, 'break_evidence'>[] {
  return result.verdicts.map(({ break_evidence: _ignored, ...rest }) => rest);
}

describe('check_v2: the relaxed re-check is evidence only (incompatible-rule T1)', () => {
  let working: CheckResult;
  let workspace: string;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-not-proven-run-'));
    writeStub('orders', SEEDS.map((s) => `export type ${s.key}_P = ${s.producer};`).join('\n') + '\n');
    writeStub('web', SEEDS.map((s) => `export type ${s.key}_C = ${s.consumer};`).join('\n') + '\n');
    stubs = [
      { service_name: 'orders', stub_dir: path.join(root, 'orders') },
      { service_name: 'web', stub_dir: path.join(root, 'web') },
    ];
    working = await runCheck({ stubs, pairs: PAIRS, cleanup: false });
    workspace = working.workspace_dir;
  });

  after(() => {
    if (workspace) fs.rmSync(workspace, { recursive: true, force: true });
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('the judge reads the seeds as it always has', () => {
    assert.strictEqual(working.success, true, JSON.stringify(working.errors));
    const verdicts = byKey(working);
    assert.strictEqual(verdicts.get('agrees')!.bucket, 'compatible');
    assert.strictEqual(verdicts.get('anyside')!.bucket, 'gate_caught_baked_any');
    for (const key of INCOMPATIBLE) assert.strictEqual(verdicts.get(key)!.bucket, 'incompatible', key);
  });

  it('evidence on every incompatible verdict, and on no other', () => {
    for (const v of working.verdicts) {
      assert.strictEqual(v.break_evidence !== undefined, v.bucket === 'incompatible', v.pair_key);
    }
    const kinds = (key: string) => byKey(working).get(key)!.break_evidence;
    assert.deepStrictEqual(kinds('openkind'), { proven: false, kinds: ['open_value'], passing: [['open_value']] });
    assert.deepStrictEqual(kinds('nullrequired'), { proven: false, kinds: ['null_where_required'], passing: [['null_where_required']] });
    assert.deepStrictEqual(kinds('weakcast'), { proven: false, kinds: ['weak_type'], passing: [['weak_type']] });
    assert.deepStrictEqual(kinds('socketnull'), { proven: false, kinds: ['null_where_optional'], passing: [['null_where_optional']] });
    assert.deepStrictEqual(kinds('graphqlleaf'), { proven: false, kinds: ['open_value'], passing: [['open_value']] });
    assert.deepStrictEqual(kinds('openunderdeeparrays'), { proven: false, kinds: ['open_value'], passing: [['open_value']] });
    for (const key of ['missingfield', 'missingunderdeeparrays', 'dateunderdeeparrays']) {
      assert.deepStrictEqual(kinds(key), {
        proven: true,
        kinds: [],
        passing: [],
        cause: REMAINS,
      }, key);
    }
  });

  it('relaxed probes are written for the incompatible pairs only, outside the judge\'s probes', () => {
    const probes = path.join(workspace, 'packages');
    const dirs = fs.readdirSync(probes).map((d) => path.join(probes, d, RELAXED_DIR)).filter((d) => fs.existsSync(d));
    assert.strictEqual(dirs.length, 1);
    const written = fs.readdirSync(dirs[0]).sort();
    const expected = working.verdicts.filter((v) => v.bucket === 'incompatible').map((v) => `pair_${v.pair_id}.ts`).sort();
    assert.deepStrictEqual(written, expected);
  });

  it('the re-check time is in its own phase', () => {
    assert.ok(Number.isInteger(working.phase_ms?.relaxed) && working.phase_ms!.relaxed >= 0);
  });

  async function failedWith(relaxedTscPath: string, relaxedTimeoutMs?: number): Promise<CheckResult> {
    const result = await runCheck({ stubs, pairs: PAIRS, relaxedTscPath, relaxedTimeoutMs });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    assert.deepStrictEqual(withoutEvidence(result), withoutEvidence(working), 'no verdict moves');
    assert.deepStrictEqual(result.errors, working.errors);
    assert.deepStrictEqual(result.degraded_services, working.degraded_services);
    for (const v of result.verdicts) {
      if (v.bucket !== 'incompatible') {
        assert.strictEqual(v.break_evidence, undefined);
        continue;
      }
      assert.strictEqual(v.break_evidence?.proven, true, v.pair_key);
      assert.deepStrictEqual(v.break_evidence.passing, []);
    }
    return result;
  }

  it('a re-check compiler that cannot be started: proven, verdicts as the judge made them', async () => {
    const result = await failedWith(path.join(root, 'no-such-tsc'));
    assert.match(result.verdicts.find((v) => v.bucket === 'incompatible')!.break_evidence!.cause!, /could not run/);
  });

  it('a re-check compiler that ends abnormally: proven', async () => {
    const stub = path.join(root, 'tsc-dies');
    fs.writeFileSync(stub, '#!/bin/sh\necho "internal error" >&2\nexit 3\n');
    fs.chmodSync(stub, 0o755);
    const result = await failedWith(stub);
    assert.match(result.verdicts.find((v) => v.bucket === 'incompatible')!.break_evidence!.cause!, /ended abnormally \(exit code 3\)/);
  });

  it('a re-check compiler that prints nothing and exits clean: proven (the canary)', async () => {
    const stub = path.join(root, 'tsc-silent');
    fs.writeFileSync(stub, '#!/bin/sh\nexit 0\n');
    fs.chmodSync(stub, 0o755);
    const result = await failedWith(stub);
    assert.match(result.verdicts.find((v) => v.bucket === 'incompatible')!.break_evidence!.cause!, /judge's own diagnostics/);
  });

  it('a re-check that runs out of time: proven', async () => {
    const stub = path.join(root, 'tsc-slow');
    fs.writeFileSync(stub, '#!/bin/sh\nsleep 20\n');
    fs.chmodSync(stub, 0o755);
    const result = await failedWith(stub, 300);
    assert.match(result.verdicts.find((v) => v.bucket === 'incompatible')!.break_evidence!.cause!, /did not finish within/);
  });
});
