/**
 * The judge's GraphQL rules, exported for the held check (S-G1).
 *
 * (1) The probe text is pinned: `buildProbe(...).source` for twelve fixed
 *     pairs equals the text captured before the rules were extracted.
 * (2) Agreement (P13): over typed pairs the judge's `compatible` and
 *     `unverifiable` are silent in the held form and its `incompatible`
 *     speaks, by the native compiler over both texts. Each GraphQL gate the
 *     held form composes (the field-resolver gate) keeps this green.
 * (3) `write_shadow` carries the rules, and a leaf only for the roots the
 *     judge reads in the scalar form.
 */
import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { buildProbe, jsonWireDeclarations, jsonWireRule, judgeTolerances } from '../src/capture/check-probe.js';
import { runCheck } from '../src/capture/index.js';
import { heldGraphql, heldWire } from '../src/held-shadow.js';
import type { CheckPairSpec, CheckStubInput, CheckVerdict } from '../src/capture/api.js';
import { FIXED_SPECS, SHOP_TYPES, typesOf } from './check-probe-held-graphql-specs.js';
import { nativeTsc, scratch } from './held-shadow-support.js';
import { SIDECAR_PATH } from './helpers.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const GOLDEN: Record<string, string> = JSON.parse(
  fs.readFileSync(
    path.resolve(here, '..', '..', 'test', 'fixtures', 'check-probe-held-graphql.golden.json'),
    'utf8'
  )
);

describe('the judge probe text is unchanged by the rule extraction', () => {
  for (const spec of FIXED_SPECS) {
    it(`${spec.pair_key}: buildProbe source equals the captured text`, () => {
      const { source } = buildProbe(spec, (n) => `@carrick/${n}`, undefined, undefined, typesOf);
      assert.equal(source, GOLDEN[spec.pair_key]);
    });
  }

  it('the golden file holds exactly the fixed specs', () => {
    assert.deepEqual(Object.keys(GOLDEN).sort(), FIXED_SPECS.map((s) => s.pair_key).sort());
  });
});

interface Case {
  key: string;
  sent: string;
  expected: string;
  root?: string;
  /** The judge's bucket for the pair. */
  bucket: 'compatible' | 'incompatible' | 'unverifiable';
}

const CASES: Case[] = [
  { key: 'typename-relaxed', root: 'order', sent: '{ id: string; total: number }', expected: "{ __typename: 'Order'; id: string; total: number }", bucket: 'compatible' },
  { key: 'typename-wrong', root: 'order', sent: "{ __typename: 'Order'; id: string }", expected: "{ __typename: 'Line'; id: string }", bucket: 'incompatible' },
  { key: 'envelope-ok', root: 'orders', sent: '{ items: { id: string }[]; count: number }', expected: '{ id: string }[]', bucket: 'compatible' },
  { key: 'envelope-break', root: 'orders', sent: '{ items: { id: string }[]; count: number }', expected: '{ id: number }[]', bucket: 'incompatible' },
  { key: 'scalar-leaf', root: 'order', sent: '{ id: bigint }', expected: '{ id: string }', bucket: 'compatible' },
  // The judge's scalar rule reads any value on a built-in leaf as that scalar's type; the held form says the same.
  // The envelope unwrap moves the leaf to the envelope field's: `id` is read as an `ID` there.
  { key: 'envelope-scalar-leaf', root: 'orderPage', sent: '{ edges: { id: bigint }[]; totalCount: number }', expected: '{ id: string }[]', bucket: 'compatible' },
  { key: 'scalar-leaf-pinned', root: 'order', sent: '{ pending: string }', expected: '{ pending: boolean }', bucket: 'compatible' },
  { key: 'custom-scalar-ok', root: 'order', sent: '{ price: Date }', expected: '{ price: string }', bucket: 'compatible' },
  { key: 'custom-scalar-break', root: 'order', sent: '{ price: number }', expected: '{ price: string }', bucket: 'incompatible' },
  { key: 'list-ok', root: 'orders', sent: '{ id: string }[]', expected: '{ id: string }[]', bucket: 'compatible' },
  { key: 'list-break', root: 'orders', sent: '{ id: string }[]', expected: '{ id: number }[]', bucket: 'incompatible' },
  { key: 'null-ok', root: 'order', sent: '{ id: string } | null', expected: '{ id: string } | null', bucket: 'compatible' },
  { key: 'null-break', root: 'order', sent: '{ id: string } | null', expected: '{ id: string }', bucket: 'incompatible' },
  { key: 'field-resolver', root: 'order', sent: '{ id: string }', expected: '{ id: string; label: string }', bucket: 'unverifiable' },
  { key: 'field-not-in-sdl', root: 'order', sent: '{ id: string }', expected: '{ id: string; missing: string }', bucket: 'incompatible' },
  { key: 'json-ok', sent: '{ at: Date }', expected: '{ at: string }', bucket: 'compatible' },
  { key: 'json-break', sent: '{ at: number }', expected: '{ at: string }', bucket: 'incompatible' },
];

function writeStub(dir: string, serviceName: string, surface: string): void {
  const stubDir = path.join(dir, serviceName);
  fs.mkdirSync(path.join(stubDir, 'types'), { recursive: true });
  fs.writeFileSync(
    path.join(stubDir, 'package.json'),
    JSON.stringify({ name: `@carrick/${serviceName}`, version: '0.0.0-carrick', private: true, types: './types/surface.d.ts' }) + '\n'
  );
  fs.writeFileSync(path.join(stubDir, 'types', 'surface.d.ts'), surface);
}

/** The held check of one pair: the engine's declarations, then the call a placed check would write. */
function heldSource(c: Case): string {
  const hg = heldGraphql(SHOP_TYPES, [{ type: 'Query', field: 'order' }, { type: 'Query', field: 'orders' }, { type: 'Query', field: 'orderPage' }]);
  const leaf = c.root === undefined ? undefined : hg.leaves[`Query.${c.root}`];
  const form = leaf === undefined ? '__carrick_GqlSent<Awaited<V>, E>' : '__carrick_GqlSentS<Awaited<V>, E, L>';
  return (
    [
      ...heldWire(),
      ...judgeTolerances('__carrick_').declarations,
      ...hg.lines,
      ...(hg.types_decl === null ? [] : [hg.types_decl]),
      `type S = ${c.sent};`,
      `type E = ${c.expected};`,
      ...(leaf === undefined ? [] : [`type L = ${JSON.stringify(leaf)};`]),
      `declare function send<V>(v: V, c: (o: ${form}) => __carrick_GqlExpected<E>): V;`,
      'declare const v: S;',
      'send(v, (o) => o);',
    ].join('\n') + '\n'
  );
}

/** The native compiler's error lines for one standalone file (strict, no libs beyond es2022). */
function tscErrors(source: string): string[] {
  const dir = scratch('held-gql-');
  const file = path.join(dir, 'a.ts');
  fs.writeFileSync(file, source);
  const run = spawnSync(nativeTsc(), ['--noEmit', '--strict', '--target', 'es2022', '--lib', 'es2022', '--ignoreConfig', '--pretty', 'false', file], {
    cwd: dir,
    encoding: 'utf8',
  });
  return run.stdout.split('\n').filter((l) => /error TS\d+/.test(l));
}

describe('the held GraphQL form agrees with the judge over typed pairs (P13)', () => {
  let root: string;
  let verdicts: Map<string, CheckVerdict>;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-held-gql-agree-'));
    writeStub(root, 'shop', CASES.map((c) => `export type S_${c.key.replace(/-/g, '_')} = ${c.sent};`).join('\n') + '\n');
    writeStub(root, 'till', CASES.map((c) => `export type E_${c.key.replace(/-/g, '_')} = ${c.expected};`).join('\n') + '\n');
    const pairs: CheckPairSpec[] = CASES.map((c) => ({
      pair_key: c.key,
      protocol: 'graphql',
      type_kind: 'response',
      producer: { service_name: 'shop', alias: `S_${c.key.replace(/-/g, '_')}` },
      consumer: { service_name: 'till', alias: `E_${c.key.replace(/-/g, '_')}` },
      ...(c.root === undefined ? {} : { graphql_root: { type: 'Query', field: c.root } }),
    }));
    const stubs: CheckStubInput[] = [
      { service_name: 'shop', stub_dir: path.join(root, 'shop'), graphql_types: SHOP_TYPES },
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
    it(`${c.key}: judge ${c.bucket}, held ${c.bucket === 'incompatible' ? 'speaks' : 'is silent'}`, () => {
      const v = verdicts.get(c.key);
      assert.ok(v, `verdict for ${c.key}`);
      assert.strictEqual(v.bucket, c.bucket, JSON.stringify(v));
      const errors = tscErrors(heldSource(c));
      if (c.bucket === 'incompatible') assert.ok(errors.length > 0, 'held should speak');
      else assert.deepEqual(errors, []);
    });
  }
});

describe('write_shadow carries the judge\'s GraphQL rules', () => {
  const FIXTURE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..', 'test', 'fixtures', 'held-node');

  function shadowAnswer(graphql?: object): Record<string, any> {
    const run = spawnSync('node', [SIDECAR_PATH], {
      cwd: os.tmpdir(),
      input:
        [
          { action: 'init', request_id: 'i', repo_root: FIXTURE, scan_root: FIXTURE },
          { action: 'write_shadow', request_id: 's', out_dir: scratch('held-gql-shadow-'), ...(graphql === undefined ? {} : { graphql }) },
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
    return answer;
  }

  it('a leaf only for the roots the judge reads in the scalar form; the table holds what they reach', () => {
    const a = shadowAnswer({
      types: SHOP_TYPES,
      roots: [
        { type: 'Query', field: 'order' },
        { type: 'Mutation', field: 'createOrder' },
        { type: 'Query', field: 'unlisted' },
      ],
    });
    assert.deepEqual(Object.keys(a.graphql.leaves).sort(), ['Mutation.createOrder', 'Query.order']);
    assert.deepEqual(a.graphql.leaves['Query.order'], { type: 'Order' });
    assert.match(a.graphql.types_decl, /^type __carrick_GqlTypes = \{"Line":/);
    assert.ok(a.graphql.lines.some((l: string) => l.startsWith('type __carrick_GqlSentS<S, E, R>')));
    assert.ok(a.graphql.lines.some((l: string) => l.startsWith('type __carrick_GqlSent<S, E>')));
  });

  it('no scalar-form root: the JSON form only, and no table is declared', () => {
    const a = shadowAnswer({ types: SHOP_TYPES, roots: [{ type: 'Query', field: 'unlisted' }] });
    assert.deepEqual(a.graphql.leaves, {});
    assert.strictEqual(a.graphql.types_decl, null);
    assert.ok(!a.graphql.lines.some((l: string) => l.includes('GqlSentS') || l.includes('GqlTypes')));
  });

  it('without a GraphQL table the answer is what it was', () => {
    const a = shadowAnswer();
    assert.strictEqual('graphql' in a, false);
    assert.deepEqual(a.wire, [...jsonWireDeclarations('__carrick_'), `type __carrick_Wire<S, E> = ${jsonWireRule('S', 'E', '__carrick_')};`]);
  });
});
