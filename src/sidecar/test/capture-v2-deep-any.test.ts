/**
 * Deep (member-level) any/unknown detection — adversarial-review finding 1,
 * the CRITICAL false-compatible.
 *
 * The self-check's top-type gate was WHOLE-type only (`flags & Any|Unknown|
 * Never` on the alias root), and the check phase's probe gates are whole-type
 * too. A captured type carrying `any` at a member/element/type-arg position —
 * `{ orderId: string; metadata: any }`, `any[]`, `Promise<any>`,
 * `Record<string, any>` — recorded self_check 'ok', probed clean, and the
 * pair read COMPATIBLE against a counterparty that constrains that position
 * to a concrete shape. `any` is bidirectionally assignable: an arbitrary
 * shape reads compatible, which is the dangerous direction.
 *
 * Pins:
 *  1. capture: member/container any|unknown records decayed_internal with a
 *     any_provenance (first finding) and a reason;
 *  2. capture: fully-resolved types (including function-typed members and
 *     unions) are NOT over-demoted;
 *  3. check: a pair whose side carries a deep `any` verdicts
 *     gate_caught_baked_any (None downstream), a deep `unknown` verdicts
 *     unverifiable — never compatible.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub, runCheck } from '../src/capture/index.js';
import type { CaptureAliasRecord, CaptureStubResult } from '../src/capture/api.js';

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
    anchors: Object.entries(literals).map(([alias, type_text]) => ({
      kind: 'literal' as const,
      alias,
      type_text,
      anchor_origin: 'deterministic-infer' as const,
    })),
  });
  assert.strictEqual(result.success, true, JSON.stringify(result.errors));
  return result;
}

/** `{ l1: { l2: ... { l<levels>: <leaf> } } }` — an `any`/`unknown` leaf sits
 * at nesting depth `levels`, past the old MAX_DEPTH=8 for levels > 8. */
function nested(levels: number, leaf: string): string {
  let t = leaf;
  for (let i = levels; i >= 1; i--) t = `{ l${i}: ${t} }`;
  return t;
}

/** `k` Array-wrapped object levels: each `lN: Array<...>` costs 2 walk hops
 * (property + type-arg), so the leaf sits at hop-depth 2k+1 — over the budget
 * at only k=8 OBJECT levels, the walk-depth-counts-every-hop severity case. */
function arrWrapped(k: number, leaf: string): string {
  let t = `{ leaf: ${leaf} }`;
  for (let i = k; i >= 1; i--) t = `{ l${i}: Array<${t}> }`;
  return t;
}

/** `n` DISTINCT-typed properties (so `seen` cannot dedup them and the node
 * budget actually climbs), the leaf at the last property. */
function wideDistinct(n: number, leaf: string): string {
  const props: string[] = [];
  for (let i = 1; i < n; i++) props.push(`p${i}: { u${i}: string }`);
  props.push(`p${n}: ${leaf}`);
  return `{ ${props.join('; ')} }`;
}

describe('capture self-check: deep any/unknown decays, concrete types do not', () => {
  let root: string;
  let byAlias: Map<string, CaptureAliasRecord>;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-deep-any-'));
    const result = captureLiterals(root, 'deep-any-svc', {
      M_MemberAny: '{ orderId: string; metadata: any }',
      M_MemberUnknown: '{ orderId: string; metadata: unknown }',
      M_NestedAny: '{ outer: { inner: { leaked: any } } }',
      C_ArrayAny: 'any[]',
      C_PromiseAny: 'Promise<any>',
      C_RecordAny: 'Record<string, any>',
      C_IndexAny: '{ [k: string]: any }',
      OK_Concrete: '{ orderId: string; metadata: { source: string } }',
      OK_Union: '{ v: string | null; w?: number }',
      OK_Array: '{ items: { sku: string }[] }',
      OK_FuncMember: '{ id: string; cb: (x: any) => void }',
    });
    byAlias = new Map(result.aliases.map((a) => [a.alias, a]));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('member-level any decays with a recorded reason and kind', () => {
    const rec = byAlias.get('M_MemberAny')!;
    assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
    assert.strictEqual(rec.any_provenance?.[0]?.kind, 'any');
    assert.strictEqual(rec.any_provenance?.[0]?.path, 'metadata');
    assert.match(rec.self_check_detail ?? '', /carries 'any'/);
    // The root is NOT a top type — that is exactly why the old gate missed it.
    assert.strictEqual(rec.top_type_at_self_check, false);
  });

  it('member-level unknown decays with kind unknown', () => {
    const rec = byAlias.get('M_MemberUnknown')!;
    assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
    assert.strictEqual(rec.any_provenance?.[0]?.kind, 'unknown');
  });

  it('nested member any is found below the first level', () => {
    const rec = byAlias.get('M_NestedAny')!;
    assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
    assert.strictEqual(rec.any_provenance?.[0]?.kind, 'any');
    assert.strictEqual(rec.any_provenance?.[0]?.path, 'outer.inner.leaked');
  });

  it('container-decayed forms (any[], Promise<any>, Record<string, any>, index) decay', () => {
    for (const alias of ['C_ArrayAny', 'C_PromiseAny', 'C_RecordAny', 'C_IndexAny']) {
      const rec = byAlias.get(alias)!;
      assert.strictEqual(rec.self_check, 'decayed_internal', `${alias}: ${rec.self_check_detail}`);
      assert.strictEqual(rec.any_provenance?.[0]?.kind, 'any', alias);
    }
  });

  it('fully-resolved types are not over-demoted', () => {
    for (const alias of ['OK_Concrete', 'OK_Union', 'OK_Array', 'OK_FuncMember']) {
      const rec = byAlias.get(alias)!;
      assert.strictEqual(rec.self_check, 'ok', `${alias}: ${rec.self_check_detail}`);
      assert.strictEqual(rec.any_provenance?.[0]?.kind, undefined, alias);
    }
  });
});

describe('capture self-check: deep any through a symbol anchor', () => {
  let symRoot: string;

  before(() => {
    symRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-deep-any-sym-'));
  });

  after(() => {
    fs.rmSync(symRoot, { recursive: true, force: true });
  });

  it('an author-level any member in captured source decays the alias', () => {
    const repoRoot = path.join(symRoot, 'repo');
    fs.mkdirSync(repoRoot, { recursive: true });
    fs.writeFileSync(
      path.join(repoRoot, 'types.ts'),
      'export interface Order { id: string; meta: any; }\n' +
        'export interface Clean { id: string; total: number; }\n'
    );
    const result = captureStub({
      repoRoot,
      serviceName: 'deep-any-symbol-svc',
      outDir: path.join(symRoot, 'stub'),
      anchors: [
        {
          kind: 'symbol',
          alias: 'P_Order',
          symbol_name: 'Order',
          source_file: 'types.ts',
          anchor_origin: 'llm-symbol',
        },
        {
          kind: 'symbol',
          alias: 'P_Clean',
          symbol_name: 'Clean',
          source_file: 'types.ts',
          anchor_origin: 'llm-symbol',
        },
      ],
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    const byAlias = new Map(result.aliases.map((a) => [a.alias, a]));
    const order = byAlias.get('P_Order')!;
    assert.strictEqual(order.self_check, 'decayed_internal', order.self_check_detail);
    assert.strictEqual(order.any_provenance?.[0]?.kind, 'any');
    assert.strictEqual(order.any_provenance?.[0]?.path, 'meta');
    const clean = byAlias.get('P_Clean')!;
    assert.strictEqual(clean.self_check, 'ok', clean.self_check_detail);
  });
});

describe('check phase: capture-recorded deep decay is never read as compatible', () => {
  let checkRoot: string;
  let producerStub: string;
  let consumerStub: string;

  before(() => {
    checkRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-deep-any-check-'));
    const producer = captureLiterals(checkRoot, 'deep-producer', {
      P_MemberAny: '{ orderId: string; metadata: any }',
      P_MemberUnknown: '{ orderId: string; metadata: unknown }',
      P_Clean: '{ orderId: string; metadata: { source: string } }',
    });
    const consumer = captureLiterals(checkRoot, 'deep-consumer', {
      C_Concrete: '{ orderId: string; metadata: { source: string } }',
      C_Concrete2: '{ orderId: string; metadata: { source: string } }',
      C_Concrete3: '{ orderId: string; metadata: { source: string } }',
    });
    producerStub = producer.stub_dir;
    consumerStub = consumer.stub_dir;
  });

  after(() => {
    fs.rmSync(checkRoot, { recursive: true, force: true });
  });

  it('deep any -> gate_caught_baked_any; deep unknown -> unverifiable; clean pair still probes', async () => {
    const result = await runCheck({
      stubs: [
        { service_name: 'deep-producer', stub_dir: producerStub },
        { service_name: 'deep-consumer', stub_dir: consumerStub },
      ],
      pairs: [
        {
          pair_key: 'any-pair',
          protocol: 'http',
          type_kind: 'response',
          producer: { service_name: 'deep-producer', alias: 'P_MemberAny' },
          consumer: { service_name: 'deep-consumer', alias: 'C_Concrete' },
        },
        {
          pair_key: 'unknown-pair',
          protocol: 'http',
          type_kind: 'response',
          producer: { service_name: 'deep-producer', alias: 'P_MemberUnknown' },
          consumer: { service_name: 'deep-consumer', alias: 'C_Concrete2' },
        },
        {
          pair_key: 'clean-pair',
          protocol: 'http',
          type_kind: 'response',
          producer: { service_name: 'deep-producer', alias: 'P_Clean' },
          consumer: { service_name: 'deep-consumer', alias: 'C_Concrete3' },
        },
      ],
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    const byKey = new Map(result.verdicts.map((v) => [v.pair_key, v]));

    // Pre-fix this read 'compatible': member-level any sails through the
    // whole-type IsAny gate and the assignment compiles clean.
    const anyVerdict = byKey.get('any-pair')!;
    assert.strictEqual(anyVerdict.bucket, 'gate_caught_baked_any', JSON.stringify(anyVerdict));
    assert.strictEqual(anyVerdict.gate, 'capture:producer:any');
    assert.match(anyVerdict.diagnostic ?? '', /metadata/);

    const unknownVerdict = byKey.get('unknown-pair')!;
    assert.strictEqual(unknownVerdict.bucket, 'unverifiable', JSON.stringify(unknownVerdict));
    assert.strictEqual(unknownVerdict.gate, 'capture:producer:unknown');

    // A fully-resolved pair still goes through the real probe path.
    const cleanVerdict = byKey.get('clean-pair')!;
    assert.strictEqual(cleanVerdict.bucket, 'compatible', JSON.stringify(cleanVerdict));
  });
});

/**
 * PR #448 adversarial-review follow-up: the deep walk missed disqualifying
 * `any` in two positions the removed `type_state == Unknown` pre-verdict gate
 * used to abstain on, so post-#448 they persisted as FALSE-COMPATIBLE:
 *   1. inside a callable — the walk WHOLESALE-skipped any type with a call
 *      signature, so `{ getData: () => any }` sailed through. A callable
 *      RETURN is a covariant wire position; `() => any` masks a real mismatch.
 *   2. past MAX_DEPTH=8 — v1's inline expander reaches MAX_EXPANSION_DEPTH=12,
 *      so a depth 9-12 `any` was flagged by v1 (→ Unknown → abstain) but
 *      missed by the walk.
 * The fix descends call/construct-signature RETURN types (params stay
 * permissive) and reconciles the depth bound to v1's expander. Positives lock
 * the no-over-demotion boundary: a clean callable return, clean deep nesting,
 * and a parameter `any` (contravariant, genuinely permissive) still verify.
 */
describe('capture self-check: callable-return any and deep-past-old-bound any', () => {
  let root: string;
  let byAlias: Map<string, CaptureAliasRecord>;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-448-callable-'));
    const result = captureLiterals(root, 'callable-svc', {
      // Finding 1: callable RETURN any (covariant wire position).
      M_RetAny: '{ getData: () => any }',
      M_CtorRetAny: '{ make: new () => any }',
      M_RetUnknown: '{ getData: () => unknown }',
      M_HybridRetAny: '{ (): any; data: string }',
      // Finding 2: any past the old MAX_DEPTH=8 (v1 expands to 12).
      M_Deep11Any: nested(11, 'any'),
      // Positives — must NOT be over-demoted:
      OK_RetClean: '{ getData: () => string }',
      OK_CtorClean: '{ make: new () => { id: string } }',
      OK_DeepClean: nested(14, 'string'),
      OK_ParamAny: '{ id: string; cb: (x: any) => void }', // contravariant, permissive
    });
    byAlias = new Map(result.aliases.map((a) => [a.alias, a]));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('a callable RETURN any decays with path <member>()', () => {
    const rec = byAlias.get('M_RetAny')!;
    assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
    assert.strictEqual(rec.any_provenance?.[0]?.kind, 'any');
    assert.strictEqual(rec.any_provenance?.[0]?.path, 'getData()');
    assert.strictEqual(rec.top_type_at_self_check, false);
  });

  it('a construct-signature RETURN any decays', () => {
    const rec = byAlias.get('M_CtorRetAny')!;
    assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
    assert.strictEqual(rec.any_provenance?.[0]?.kind, 'any');
  });

  it('a callable RETURN unknown decays with kind unknown', () => {
    const rec = byAlias.get('M_RetUnknown')!;
    assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
    assert.strictEqual(rec.any_provenance?.[0]?.kind, 'unknown');
  });

  it('a hybrid callable ({ (): any; data }) still has its call return walked', () => {
    const rec = byAlias.get('M_HybridRetAny')!;
    assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
    assert.strictEqual(rec.any_provenance?.[0]?.kind, 'any');
  });

  it('an any at depth 11 (past the old MAX_DEPTH=8) decays', () => {
    const rec = byAlias.get('M_Deep11Any')!;
    assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
    assert.strictEqual(rec.any_provenance?.[0]?.kind, 'any');
    assert.match(rec.any_provenance?.[0]?.path ?? '', /l11$/);
  });

  it('clean callable returns, clean deep nesting, and a parameter any are NOT over-demoted', () => {
    for (const alias of ['OK_RetClean', 'OK_CtorClean', 'OK_DeepClean', 'OK_ParamAny']) {
      const rec = byAlias.get(alias)!;
      assert.strictEqual(rec.self_check, 'ok', `${alias}: ${rec.self_check_detail}`);
      assert.strictEqual(rec.any_provenance?.[0]?.kind, undefined, alias);
    }
  });
});

describe('check phase: callable-return any and deep-past-8 any are never compatible', () => {
  let checkRoot: string;
  let producerStub: string;
  let consumerStub: string;

  before(() => {
    checkRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-448-callable-check-'));
    const producer = captureLiterals(checkRoot, 'cb-producer', {
      P_RetAny: '{ getData: () => any }',
      P_Deep11Any: nested(11, 'any'),
      P_RetClean: '{ getData: () => string }',
      P_DeepClean: nested(11, 'string'),
    });
    const consumer = captureLiterals(checkRoot, 'cb-consumer', {
      C_RetClean1: '{ getData: () => string }',
      C_Deep11: nested(11, 'string'),
      C_RetClean2: '{ getData: () => string }',
      C_Deep11b: nested(11, 'string'),
    });
    producerStub = producer.stub_dir;
    consumerStub = consumer.stub_dir;
  });

  after(() => {
    fs.rmSync(checkRoot, { recursive: true, force: true });
  });

  it('callable-return any -> gated; deep-past-8 any -> gated; clean counterparts still verify', async () => {
    const result = await runCheck({
      stubs: [
        { service_name: 'cb-producer', stub_dir: producerStub },
        { service_name: 'cb-consumer', stub_dir: consumerStub },
      ],
      pairs: [
        {
          pair_key: 'ret-any',
          protocol: 'http',
          type_kind: 'response',
          producer: { service_name: 'cb-producer', alias: 'P_RetAny' },
          consumer: { service_name: 'cb-consumer', alias: 'C_RetClean1' },
        },
        {
          pair_key: 'deep11-any',
          protocol: 'http',
          type_kind: 'response',
          producer: { service_name: 'cb-producer', alias: 'P_Deep11Any' },
          consumer: { service_name: 'cb-consumer', alias: 'C_Deep11' },
        },
        {
          pair_key: 'ret-clean',
          protocol: 'http',
          type_kind: 'response',
          producer: { service_name: 'cb-producer', alias: 'P_RetClean' },
          consumer: { service_name: 'cb-consumer', alias: 'C_RetClean2' },
        },
        {
          pair_key: 'deep-clean',
          protocol: 'http',
          type_kind: 'response',
          producer: { service_name: 'cb-producer', alias: 'P_DeepClean' },
          consumer: { service_name: 'cb-consumer', alias: 'C_Deep11b' },
        },
      ],
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    const byKey = new Map(result.verdicts.map((v) => [v.pair_key, v]));

    // Pre-fix these read 'compatible' (walk skipped the callable / capped at 8).
    assert.strictEqual(byKey.get('ret-any')!.bucket, 'gate_caught_baked_any', JSON.stringify(byKey.get('ret-any')));
    assert.strictEqual(byKey.get('deep11-any')!.bucket, 'gate_caught_baked_any', JSON.stringify(byKey.get('deep11-any')));
    // Clean counterparts must STILL verify — no over-demotion.
    assert.strictEqual(byKey.get('ret-clean')!.bucket, 'compatible', JSON.stringify(byKey.get('ret-clean')));
    assert.strictEqual(byKey.get('deep-clean')!.bucket, 'compatible', JSON.stringify(byKey.get('deep-clean')));
  });
});

/**
 * PR #448 adversarial-review follow-up, hole 3: `blamedExternal` masking. On a
 * BARE checkout, an alias whose closure has a failing pinned external was
 * classified `allowlisted_external` and its first `any_provenance` finding was NOT
 * recorded — so an AUTHOR-baked deep `any` co-present with that external was
 * suppressed and read compatible at check.
 *
 * The fix: the deep walk excludes TypeScript's unresolved-reference `error`
 * placeholder (`intrinsicName === 'error'`, e.g. `import('ext').Foo` on a bare
 * checkout — which heals when the check installs the pin) and counts only a
 * genuine author `any`/`unknown`. A found deep decay is then recorded even when
 * a (healable) external is co-present. A pure external decay, with no author
 * any, still allowlists — no over-demotion.
 */
describe('capture self-check: author-baked deep any is not masked by a co-present external', () => {
  let root: string;
  let byAlias: Map<string, CaptureAliasRecord>;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-448-blamed-ext-'));
    const repoRoot = path.join(root, 'repo');
    fs.mkdirSync(repoRoot, { recursive: true });
    fs.writeFileSync(
      path.join(repoRoot, 'types.ts'),
      "import type { Widget } from 'some-ext';\n" +
        'export interface PureExt { id: string; w: Widget; }\n' +
        'export interface AuthorAny { id: string; w: Widget; getData: () => any; }\n'
    );
    fs.writeFileSync(
      path.join(repoRoot, 'package.json'),
      JSON.stringify({ name: 'p', version: '1.0.0', dependencies: { 'some-ext': '1.2.3' } })
    );
    // Bare checkout (no node_modules) + npm lockfile pins `some-ext`, so its
    // unresolved reference is a blamedExternal (allowlistable), not an unpinned
    // internal failure.
    fs.writeFileSync(
      path.join(repoRoot, 'package-lock.json'),
      JSON.stringify({
        lockfileVersion: 3,
        packages: { '': {}, 'node_modules/some-ext': { version: '1.2.3' } },
      })
    );
    const result = captureStub({
      repoRoot,
      serviceName: 'blamed-ext-svc',
      outDir: path.join(root, 'stub'),
      anchors: [
        { kind: 'symbol', alias: 'PureExt', symbol_name: 'PureExt', source_file: 'types.ts', anchor_origin: 'llm-symbol' },
        { kind: 'symbol', alias: 'AuthorAny', symbol_name: 'AuthorAny', source_file: 'types.ts', anchor_origin: 'llm-symbol' },
      ],
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    assert.strictEqual(result.bare_checkout, true);
    assert.deepStrictEqual(result.pinned_dependencies, { 'some-ext': '1.2.3' });
    byAlias = new Map(result.aliases.map((a) => [a.alias, a]));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('an author-baked deep any co-present with a failing external is recorded (not allowlisted)', () => {
    const rec = byAlias.get('AuthorAny')!;
    assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
    assert.strictEqual(rec.any_provenance?.[0]?.kind, 'any');
    assert.strictEqual(rec.any_provenance?.[0]?.path, 'getData()');
  });

  it('a PURE external decay (no author any) still allowlists — the unresolved reference heals at check', () => {
    const rec = byAlias.get('PureExt')!;
    assert.strictEqual(rec.self_check, 'allowlisted_external', rec.self_check_detail);
    assert.strictEqual(rec.any_provenance?.[0]?.kind, undefined);
  });
});

/**
 * `flagOf` excludes TypeScript's unresolved-reference `error` placeholder so it
 * is not conflated with an author-baked `any` (hole 3). That exclusion is the
 * only NARROWING in the fix, so it must not open a new false-compatible: a
 * NON-healable error cause (an UNPINNED external, or a dangling INTERNAL
 * specifier) at a nested position — which the walk used to flag structurally —
 * must still gate, now via the closure `internalFailure`/`blamedExternal` path
 * rather than the deep walk. Only a PINNED external heals at check; these do
 * not, so they must read `decayed_internal`, never `ok`/`allowlisted_external`.
 */
describe('capture self-check: a non-healable error-type member still gates (narrowing is safe)', () => {
  let root: string;
  let byAlias: Map<string, CaptureAliasRecord>;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-448-errsafe-'));
    const repoRoot = path.join(root, 'repo');
    fs.mkdirSync(repoRoot, { recursive: true });
    // NO lockfile: `some-ext` is UNPINNED (its decay never heals at check).
    fs.writeFileSync(
      path.join(repoRoot, 'types.ts'),
      "import type { Widget } from 'some-ext';\n" +
        'export interface UnpinnedExt { id: string; deep: { w: Widget } }\n' +
        "export interface DanglingInternal { id: string; deep: { x: import('./missing').X } }\n"
    );
    fs.writeFileSync(
      path.join(repoRoot, 'package.json'),
      JSON.stringify({ name: 'p', version: '1.0.0' })
    );
    const result = captureStub({
      repoRoot,
      serviceName: 'errsafe-svc',
      outDir: path.join(root, 'stub'),
      anchors: [
        { kind: 'symbol', alias: 'UnpinnedExt', symbol_name: 'UnpinnedExt', source_file: 'types.ts', anchor_origin: 'llm-symbol' },
        { kind: 'symbol', alias: 'DanglingInternal', symbol_name: 'DanglingInternal', source_file: 'types.ts', anchor_origin: 'llm-symbol' },
      ],
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    assert.strictEqual(result.bare_checkout, true);
    // The external is genuinely unpinned (no lockfile), so it is never
    // allowlistable — it must gate as an internal failure.
    assert.deepStrictEqual(result.pinned_dependencies, {});
    byAlias = new Map(result.aliases.map((a) => [a.alias, a]));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('a nested UNPINNED-external decay gates decayed_internal (not ok/allowlisted)', () => {
    const rec = byAlias.get('UnpinnedExt')!;
    assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
  });

  it('a nested dangling-internal decay gates decayed_internal', () => {
    const rec = byAlias.get('DanglingInternal')!;
    assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
  });
});

/**
 * PR #448 adversarial-review follow-up (part C): the deep walk's budget guard
 * FAILED OPEN — `depth > MAX_DEPTH || visited > MAX_VISITED` returned undefined
 * ("clean"), so an `any` buried past the budget recorded `self_check=ok` and,
 * since the check's probe gates are whole-type only, read COMPATIBLE. Raising
 * the bound only relocated the cliff. Now budget exhaustion FAILS CLOSED: it
 * returns a `budget_exhausted` sentinel → `decayed_internal` → the check gates
 * it `unverifiable`. Because walk-depth counts every structural hop (property,
 * type-arg, union member, callable return), the cliff is reachable at only ~8
 * object levels when each wraps an `Array<…>` — an ordinary paginated generic.
 *
 * The tradeoff (owned, not hidden): a legitimately clean type deeper/wider than
 * the budget now ABSTAINS (Unverifiable) instead of verifying — the correct
 * fail-closed direction (over-abstain, never false-compatible). The budget is
 * kept generous enough that ordinary types (incl. arrays of objects, whose
 * built-in method suite is skipped, not recursed) clear it.
 */
describe('capture self-check: budget exhaustion fails CLOSED', () => {
  let root: string;
  let byAlias: Map<string, CaptureAliasRecord>;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-448-budget-'));
    const result = captureLiterals(root, 'budget-svc', {
      D17_any: nested(17, 'any'), // past MAX_DEPTH=16
      D20_any: nested(20, 'any'),
      Arr8_any: arrWrapped(8, 'any'), // leaf at hop-depth 17 via only 8 object levels
      W5000_any: wideDistinct(5000, 'any'), // past MAX_VISITED=4096
      // Controls / positives — within budget, must NOT budget-exhaust:
      OK_D16_any: nested(16, 'any'), // caught as a real 'any', not budget
      OK_ArrayObjs: '{ items: { sku: string }[] }', // lib method suite skipped, not recursed
      OK_NestedArrClean: arrWrapped(6, '{ ok: string }'), // clean array nesting within budget
      OK_D16_clean: nested(16, 'string'),
    });
    byAlias = new Map(result.aliases.map((a) => [a.alias, a]));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('a depth-17 any (past MAX_DEPTH) gates budget_exhausted, never clean', () => {
    const rec = byAlias.get('D17_any')!;
    assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
    assert.strictEqual(rec.any_provenance?.[0]?.kind, 'budget_exhausted');
  });

  it('a depth-20 any gates budget_exhausted', () => {
    assert.strictEqual(byAlias.get('D20_any')!.any_provenance?.[0]?.kind, 'budget_exhausted');
  });

  it('8 Array-wrapped object levels (hop-depth 17) gates budget_exhausted', () => {
    const rec = byAlias.get('Arr8_any')!;
    assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
    assert.strictEqual(rec.any_provenance?.[0]?.kind, 'budget_exhausted');
  });

  it('a >4096-wide any gates budget_exhausted', () => {
    assert.strictEqual(byAlias.get('W5000_any')!.any_provenance?.[0]?.kind, 'budget_exhausted');
  });

  it('a depth-16 any WITHIN budget is caught as a real any (not budget_exhausted)', () => {
    const rec = byAlias.get('OK_D16_any')!;
    assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
    assert.strictEqual(rec.any_provenance?.[0]?.kind, 'any');
  });

  it('ordinary arrays of objects and clean nested arrays are NOT over-demoted', () => {
    for (const alias of ['OK_ArrayObjs', 'OK_NestedArrClean', 'OK_D16_clean']) {
      const rec = byAlias.get(alias)!;
      assert.strictEqual(rec.self_check, 'ok', `${alias}: ${rec.self_check_detail}`);
      assert.strictEqual(rec.any_provenance?.[0]?.kind, undefined, alias);
    }
  });
});

/**
 * The seen-set/budget interaction the coordinator asked to prove, not just
 * reason: budget exhaustion fails closed, so a truncated visit returns the
 * sentinel (which bubbles up and terminates the walk) rather than being
 * memoized clean in `seen`. So a shared type buried past the budget on ONE
 * reference can never be marked clean for a SHALLOWER reference, and a cyclic
 * type terminates and still gates its buried any — while a genuinely clean
 * cyclic type is not over-demoted. Symbol anchors give the shared/cyclic types
 * a stable identity (a literal would be structurally re-created each time).
 */
describe('capture self-check: shared + cyclic types gate (seen-set is not fail-open)', () => {
  let symRoot: string;
  let byAlias: Map<string, CaptureAliasRecord>;

  before(() => {
    symRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-448-seen-'));
    const repoRoot = path.join(symRoot, 'repo');
    fs.mkdirSync(repoRoot, { recursive: true });
    const wrap = (n: number, inner: string): string => {
      let t = inner;
      for (let i = n; i >= 1; i--) t = `{ w${i}: ${t} }`;
      return t;
    };
    fs.writeFileSync(
      path.join(repoRoot, 'types.ts'),
      // `S` buries an any; referenced both DEEP (past budget) and SHALLOW.
      'export interface S { mid: { buried: any } }\n' +
        `export interface T_DeepFirst { deep: ${wrap(14, '{ s: S }')}; shallow: { s: S } }\n` +
        `export interface T_ShallowFirst { shallow: { s: S }; deep: ${wrap(14, '{ s: S }')} }\n` +
        // Cyclic type with a buried any: must terminate AND gate.
        'export interface CyclicAny { next: CyclicAny; buried: any }\n' +
        // Clean cyclic type: must terminate and NOT be over-demoted.
        'export interface CyclicClean { self: CyclicClean; data: { ok: string } }\n'
    );
    const result = captureStub({
      repoRoot,
      serviceName: 'seen-svc',
      outDir: path.join(symRoot, 'stub'),
      anchors: ['T_DeepFirst', 'T_ShallowFirst', 'CyclicAny', 'CyclicClean'].map((n) => ({
        kind: 'symbol' as const,
        alias: n,
        symbol_name: n,
        source_file: 'types.ts',
        anchor_origin: 'llm-symbol' as const,
      })),
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    byAlias = new Map(result.aliases.map((a) => [a.alias, a]));
  });

  after(() => {
    fs.rmSync(symRoot, { recursive: true, force: true });
  });

  it('a shared type buried past budget gates regardless of reference order (deep first)', () => {
    const rec = byAlias.get('T_DeepFirst')!;
    assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
    // Never `ok`: the truncated deep visit is not memoized clean for the
    // shallow reference. (Deep-first truncates -> budget_exhausted.)
    assert.ok(
      rec.any_provenance?.[0]?.kind === 'budget_exhausted' || rec.any_provenance?.[0]?.kind === 'any',
      `expected a decay kind, got ${rec.any_provenance?.[0]?.kind}`
    );
  });

  it('the same shared type gates shallow-first too (any found before the deep reference)', () => {
    const rec = byAlias.get('T_ShallowFirst')!;
    assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
    assert.ok(rec.any_provenance?.[0]?.kind === 'any' || rec.any_provenance?.[0]?.kind === 'budget_exhausted');
  });

  it('a cyclic type with a buried any terminates and gates (never ok/hang)', () => {
    const rec = byAlias.get('CyclicAny')!;
    assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
    assert.strictEqual(rec.any_provenance?.[0]?.kind, 'any');
  });

  it('a genuinely clean cyclic type terminates and is NOT over-demoted', () => {
    const rec = byAlias.get('CyclicClean')!;
    assert.strictEqual(rec.self_check, 'ok', rec.self_check_detail);
    assert.strictEqual(rec.any_provenance?.[0]?.kind, undefined);
  });
});

describe('check phase: a budget-exhausted deep any is unverifiable, within-budget clean verifies', () => {
  let checkRoot: string;
  let producerStub: string;
  let consumerStub: string;

  before(() => {
    checkRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-448-budget-check-'));
    const producer = captureLiterals(checkRoot, 'bd-producer', {
      P_D17any: nested(17, 'any'),
      P_D16clean: nested(16, 'string'),
      P_ShallowClean: '{ ok: string }', // shallow clean producer for the consumer-side case
    });
    const consumer = captureLiterals(checkRoot, 'bd-consumer', {
      C_D17: nested(17, 'string'),
      C_D16: nested(16, 'string'),
      C_D17any: nested(17, 'any'), // budget-exhausted on the CONSUMER side
    });
    producerStub = producer.stub_dir;
    consumerStub = consumer.stub_dir;
  });

  after(() => {
    fs.rmSync(checkRoot, { recursive: true, force: true });
  });

  it('depth-17 any -> unverifiable (budget_exhausted); depth-16 clean -> compatible', async () => {
    const result = await runCheck({
      stubs: [
        { service_name: 'bd-producer', stub_dir: producerStub },
        { service_name: 'bd-consumer', stub_dir: consumerStub },
      ],
      pairs: [
        {
          pair_key: 'd17',
          protocol: 'http',
          type_kind: 'response',
          producer: { service_name: 'bd-producer', alias: 'P_D17any' },
          consumer: { service_name: 'bd-consumer', alias: 'C_D17' },
        },
        {
          pair_key: 'd16-clean',
          protocol: 'http',
          type_kind: 'response',
          producer: { service_name: 'bd-producer', alias: 'P_D16clean' },
          consumer: { service_name: 'bd-consumer', alias: 'C_D16' },
        },
        {
          // Symmetric: the budget-exhausted any on the CONSUMER side.
          pair_key: 'd17-consumer',
          protocol: 'http',
          type_kind: 'response',
          producer: { service_name: 'bd-producer', alias: 'P_ShallowClean' },
          consumer: { service_name: 'bd-consumer', alias: 'C_D17any' },
        },
      ],
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    const byKey = new Map(result.verdicts.map((v) => [v.pair_key, v]));
    // Pre-fix this read 'compatible' — the any past the budget looked clean.
    const d17 = byKey.get('d17')!;
    assert.strictEqual(d17.bucket, 'unverifiable', JSON.stringify(d17));
    assert.strictEqual(d17.gate, 'capture:producer:budget_exhausted');
    // Symmetric on the consumer side (deepDecayOf loops both sides).
    const d17c = byKey.get('d17-consumer')!;
    assert.strictEqual(d17c.bucket, 'unverifiable', JSON.stringify(d17c));
    assert.strictEqual(d17c.gate, 'capture:consumer:budget_exhausted');
    // A clean type within budget still verifies — no over-demotion.
    assert.strictEqual(byKey.get('d16-clean')!.bucket, 'compatible', JSON.stringify(byKey.get('d16-clean')));
  });
});
