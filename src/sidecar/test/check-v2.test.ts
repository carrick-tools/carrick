/**
 * v2 check core — end-to-end integration (real vendored pnpm + real tsc CLI).
 *
 * Hand-authored stub packages with no external dependencies (so the install is
 * local-only and network-free) exercise the whole runCheck pipeline: workspace
 * assembly, pnpm install, probe generation, the tsc judge, and the four-bucket
 * classifier. Pins each of the four buckets and byte-stability across two runs
 * (the acceptance criteria for WP2).
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runCheck } from '../src/capture/index.js';
import type { CheckPairSpec, CheckStubInput, CheckVerdict } from '../src/capture/api.js';

let root: string;
let stubs: CheckStubInput[];

function writeStub(dir: string, serviceName: string, surface: string): void {
  const stubDir = path.join(dir, serviceName);
  fs.mkdirSync(path.join(stubDir, 'types'), { recursive: true });
  fs.writeFileSync(
    path.join(stubDir, 'package.json'),
    JSON.stringify(
      {
        name: `@carrick/${serviceName}`,
        version: '0.0.0-carrick',
        private: true,
        types: './types/surface.d.ts',
      },
      null,
      2
    ) + '\n'
  );
  fs.writeFileSync(path.join(stubDir, 'types', 'surface.d.ts'), surface);
}

function mk(
  pair_key: string,
  producerAlias: string,
  consumerAlias: string,
  over: Partial<CheckPairSpec> = {}
): CheckPairSpec {
  return {
    pair_key,
    protocol: 'http',
    type_kind: 'response',
    producer: { service_name: 'orders', alias: producerAlias },
    consumer: { service_name: 'web', alias: consumerAlias },
    ...over,
  };
}

const PAIRS: CheckPairSpec[] = [
  mk('compatible', 'C_Sent', 'C_Exp'),
  mk('incompatible', 'I_Sent', 'I_Exp'),
  mk('unverifiable', 'U_Sent', 'U_Exp'),
  mk('bakedany', 'A_Sent', 'A_Exp'),
  // HTTP request-body: sent=consumer(subset), expected=producer(superset) =>
  // the consumer body cannot satisfy the producer's required field => incompatible.
  mk('reqdir', 'Req_Superset', 'Req_Subset', { type_kind: 'request' }),
  // carrick#1162: a consumer that reads no body is not a contract to compare.
  mk('voidconsumer', 'V_Sent', 'V_Exp'),
  mk('undefinedconsumer', 'V_Sent', 'Ud_Exp'),
  // carrick#1162: a form-encoded body carries its fields as runtime appends.
  mk('formbody', 'Form_Expected', 'Form_Sent', { type_kind: 'request' }),
  // An `any` side keeps its top-type reason; it is not "reads no body".
  mk('anyconsumer', 'V_Sent', 'Any_Exp'),
  mk('anyrequest', 'Form_Expected', 'Any_Exp', { type_kind: 'request' }),
  // The known true positive: a free `string` sent where a union is required.
  mk('unionrequest', 'Union_Expected', 'Union_Sent', { type_kind: 'request' }),
];

function byKey(verdicts: CheckVerdict[]): Map<string, CheckVerdict> {
  return new Map(verdicts.map((v) => [v.pair_key, v]));
}

describe('check_v2 core: four buckets + determinism (real pnpm + tsc)', () => {
  let verdicts: Map<string, CheckVerdict>;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-check-v2-stubs-'));
    writeStub(
      root,
      'orders',
      [
        'export type C_Sent = { a: string; };',
        'export type I_Sent = { a: string; };',
        'export type U_Sent = unknown;',
        'export type A_Sent = any;',
        'export type Req_Superset = { a: string; b: number; };',
        'export type V_Sent = { a: string; };',
        'export type Form_Expected = { title: string; content: string; };',
        'export type Union_Expected = { type: "boolean" | "file_upload" | "text_input"; };',
      ].join('\n') + '\n'
    );
    writeStub(
      root,
      'web',
      [
        'export type C_Exp = { a: string; };',
        'export type I_Exp = { a: string; b: number; };',
        'export type U_Exp = { a: string; };',
        'export type A_Exp = { a: string; };',
        'export type Req_Subset = { a: string; };',
        'export type V_Exp = void;',
        'export type Ud_Exp = undefined;',
        'export type Form_Sent = FormData;',
        'export type Union_Sent = { type: string; };',
        'export type Any_Exp = any;',
      ].join('\n') + '\n'
    );
    stubs = [
      { service_name: 'orders', stub_dir: path.join(root, 'orders') },
      { service_name: 'web', stub_dir: path.join(root, 'web') },
    ];
    const result = await runCheck({ stubs, pairs: PAIRS });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    assert.strictEqual(result.isolation, 'pnpm');
    assert.strictEqual(result.install_ok, true);
    verdicts = byKey(result.verdicts);
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('bucket 1 — compatible: identical/assignable types', () => {
    const v = verdicts.get('compatible')!;
    assert.strictEqual(v.bucket, 'compatible');
    assert.strictEqual(v.diagnostic, undefined);
  });

  // carrick#707 R1d. The unit tests drive the classifier directly; this is the
  // only place the walk runs where the real check runs it — a pnpm-isolated
  // workspace whose surface packages resolve through node_modules symlinks and
  // CHECKER_TSCONFIG, not a hand-written `paths` map. If it silently fails to
  // resolve the aliases there, every verdict ships `resolved: false` and the
  // field is dead while every test still passes.
  it('a compared pair with two fully known types is a fact', () => {
    const v = verdicts.get('compatible')!;
    assert.strictEqual(
      v.resolved,
      true,
      `the walk must run in the real workspace; got: ${v.unresolved_reason}`
    );
    assert.strictEqual(v.unresolved_reason, undefined);
  });

  it('a gated verdict is never a fact, and says which side', () => {
    const v = verdicts.get('bakedany')!;
    assert.strictEqual(v.resolved, false);
    assert.match(v.unresolved_reason!, /producer/);
  });

  it('bucket 2 — incompatible: real TS text is the report', () => {
    const v = verdicts.get('incompatible')!;
    assert.strictEqual(v.bucket, 'incompatible');
    assert.match(v.diagnostic!, /Property 'b' is missing/);
    assert.ok(v.codes.includes(2741));
  });

  it('bucket 3 — unverifiable: a side decayed to unknown (gate fired)', () => {
    const v = verdicts.get('unverifiable')!;
    assert.strictEqual(v.bucket, 'unverifiable');
    assert.strictEqual(v.gate, 'producer:unknown');
  });

  it('bucket 4 — gate_caught_baked_any: a side is any (IsAny gate)', () => {
    const v = verdicts.get('bakedany')!;
    assert.strictEqual(v.bucket, 'gate_caught_baked_any');
    assert.strictEqual(v.gate, 'producer:any');
  });

  it('HTTP request-body direction is inverted (consumer <= producer)', () => {
    // With the buggy producer<=consumer direction this pair would read
    // compatible; the direction table makes it correctly incompatible.
    const v = verdicts.get('reqdir')!;
    assert.strictEqual(v.bucket, 'incompatible');
  });

  it('a consumer response of void or undefined is not compared (carrick#1162)', () => {
    for (const key of ['voidconsumer', 'undefinedconsumer']) {
      const v = verdicts.get(key)!;
      assert.strictEqual(v.bucket, 'unverifiable', key);
      assert.strictEqual(v.gate, 'consumer:void', key);
      assert.strictEqual(v.resolved, false, key);
      assert.match(v.unresolved_reason!, /consumer/, key);
    }
  });

  it('a form-encoded consumer body is not compared (carrick#1162)', () => {
    const v = verdicts.get('formbody')!;
    assert.strictEqual(v.bucket, 'unverifiable');
    assert.strictEqual(v.gate, 'consumer:form');
    assert.strictEqual(v.resolved, false);
  });

  it('an any consumer keeps its top-type gate, not the void or form gate (carrick#1162)', () => {
    for (const key of ['anyconsumer', 'anyrequest']) {
      const v = verdicts.get(key)!;
      assert.strictEqual(v.bucket, 'gate_caught_baked_any', key);
      assert.strictEqual(v.gate, 'consumer:any', key);
    }
  });

  it('keeps the true positive: a string sent where a union is required', () => {
    const v = verdicts.get('unionrequest')!;
    assert.strictEqual(v.bucket, 'incompatible');
    assert.match(v.diagnostic!, /type/);
  });

  it('diagnostics carry no absolute paths or scan internals', () => {
    for (const v of verdicts.values()) {
      if (!v.diagnostic) continue;
      assert.ok(!/\/(private\/)?tmp\//.test(v.diagnostic), v.diagnostic);
      assert.ok(!v.diagnostic.includes(os.tmpdir()), v.diagnostic);
    }
  });

  it('verdicts are byte-stable across two independent runs', async () => {
    const a = await runCheck({ stubs, pairs: PAIRS });
    const b = await runCheck({ stubs, pairs: PAIRS });
    assert.strictEqual(
      JSON.stringify(a.verdicts),
      JSON.stringify(b.verdicts),
      'verdict payloads must be byte-identical'
    );
  });
});
