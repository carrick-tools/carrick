/**
 * v2 check core — pure-unit tests (no pnpm, no tsc).
 *
 * Pins the deterministic pieces the byte-stable end-to-end runner depends on:
 * the (protocol, type_kind) direction table (incl. the confirmed HTTP
 * request-body inversion), stable pair IDs, the four-bucket classifier
 * precedence, the diagnostic scrub, and semver-dedupe override computation.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import {
  buildProbe,
  directionFor,
  fnv1a,
  pairId,
} from '../src/capture/check-probe.js';
import { classifyPair, parseTscOutput } from '../src/capture/check-classify.js';
import { scrubPaths, rewriteAliases } from '../src/capture/check-scrub.js';
import { assembleWorkspace, computeDedupeOverrides } from '../src/capture/check-workspace.js';
import type { CheckPairSpec } from '../src/capture/api.js';
import type { RawDiagnostic } from '../src/capture/check-classify.js';

const PKG = (s: string) => `@carrick/${s}`;

function spec(over: Partial<CheckPairSpec> = {}): CheckPairSpec {
  return {
    pair_key: 'k1',
    protocol: 'http',
    type_kind: 'response',
    producer: { service_name: 'orders', alias: 'Endpoint_a_Response' },
    consumer: { service_name: 'web', alias: 'Call_a_Response' },
    ...over,
  };
}

describe('direction table (protocol, type_kind)', () => {
  it('http/graphql response: sent=producer, expected=consumer', () => {
    assert.deepStrictEqual(directionFor('http', 'response'), {
      sent: 'producer',
      expected: 'consumer',
    });
    assert.deepStrictEqual(directionFor('graphql', 'response'), {
      sent: 'producer',
      expected: 'consumer',
    });
  });

  it('http request INVERTS: sent=consumer, expected=producer (the confirmed bug fix)', () => {
    assert.deepStrictEqual(directionFor('http', 'request'), {
      sent: 'consumer',
      expected: 'producer',
    });
  });

  it('socket/pubsub: sent=consumer(publisher), expected=producer(subscriber)', () => {
    assert.deepStrictEqual(directionFor('socket', 'both'), {
      sent: 'consumer',
      expected: 'producer',
    });
    assert.deepStrictEqual(directionFor('pubsub', 'both'), {
      sent: 'consumer',
      expected: 'producer',
    });
  });
});

describe('pair IDs', () => {
  it('are deterministic and stable across calls', () => {
    assert.strictEqual(pairId(spec()), pairId(spec()));
  });
  it('differ when the pair identity differs', () => {
    assert.notStrictEqual(pairId(spec()), pairId(spec({ pair_key: 'k2' })));
    assert.notStrictEqual(
      pairId(spec()),
      pairId(spec({ producer: { service_name: 'orders', alias: 'Other' } }))
    );
  });
  it('fnv1a is a stable 8-hex hash', () => {
    assert.match(fnv1a('abc'), /^[0-9a-f]{8}$/);
    assert.strictEqual(fnv1a('abc'), fnv1a('abc'));
  });
});

describe('graphql probe shape (resolver-return envelope unwrap)', () => {
  it('graphql pairs assign a comparand that unwraps a single-payload envelope', () => {
    const plan = buildProbe(spec({ protocol: 'graphql' }), PKG);
    assert.ok(plan.source.includes('type GqlComparand ='), plan.source);
    assert.ok(plan.source.includes('const expected: Expected = sentComparand;'));
    // The comparand short-circuits to Sent when it already satisfies Expected
    // (plain subset selection), and falls back to Sent when no unambiguous
    // single payload property exists.
    assert.ok(plan.source.includes('[Sent] extends [Expected] ? Sent'));
    // Comparand re-gates (the v2 port of v1's post-unwrap top-type re-guard)
    // classify through the standard sent-side gate names.
    const names = [...plan.gateLines.values()];
    assert.strictEqual(names.filter((n) => n === 'sent:any').length, 2);
    assert.strictEqual(names.filter((n) => n === 'sent:unknown').length, 2);
    assert.strictEqual(names.filter((n) => n === 'sent:never').length, 2);
    // The assignment line bookkeeping still points at the real assignment.
    const lines = plan.source.split('\n');
    assert.strictEqual(lines[plan.assignmentLine - 1], 'const expected: Expected = sentComparand;');
  });

  it('non-graphql pairs keep the raw sent assignment and six gates', () => {
    for (const protocol of ['http', 'socket', 'pubsub'] as const) {
      const plan = buildProbe(spec({ protocol }), PKG);
      assert.ok(!plan.source.includes('GqlComparand'), protocol);
      assert.strictEqual(plan.gateLines.size, 6, protocol);
      const lines = plan.source.split('\n');
      assert.strictEqual(lines[plan.assignmentLine - 1], 'const expected: Expected = sent;');
    }
  });
});

describe('four-bucket classifier precedence', () => {
  const plan = buildProbe(spec(), PKG);
  const scrubCtx = { workspaceRoot: '/tmp/ws', packageLabelOf: () => undefined };
  const noPoison = () => undefined;

  const diag = (line: number, code: number, message = 'x'): RawDiagnostic => ({
    file: `packages/carrick-probes/probes/${plan.fileName}`,
    line,
    col: 1,
    code,
    message,
  });

  it('no diagnostics -> compatible', () => {
    const v = classifyPair({ plan, probeDiags: [], poisonReason: noPoison, scrubCtx });
    assert.strictEqual(v.bucket, 'compatible');
    assert.strictEqual(v.diagnostic, undefined);
  });

  // carrick#707 R1d: `compatible` says the probe raised no diagnostic.
  // Whether that is a FACT about two known types is a separate question the
  // whole-type gates cannot answer, so it is a separate field.
  describe('verdict fact-ness (resolved)', () => {
    const clean = { sent: [], expected: [] };

    it('a compared pair with a clean deep walk on both sides is a fact', () => {
      const v = classifyPair({
        plan,
        probeDiags: [],
        poisonReason: noPoison,
        scrubCtx,
        deepFindings: clean,
      });
      assert.strictEqual(v.bucket, 'compatible');
      assert.strictEqual(v.resolved, true);
      assert.strictEqual(v.unresolved_reason, undefined);
    });

    it('an incompatible verdict is equally a fact when both sides are known', () => {
      const v = classifyPair({
        plan,
        probeDiags: [diag(plan.assignmentLine, 2741, "Property 'b' is missing in type 'X'.")],
        poisonReason: noPoison,
        scrubCtx,
        deepFindings: clean,
      });
      assert.strictEqual(v.bucket, 'incompatible');
      assert.strictEqual(v.resolved, true);
    });

    it('a member-level any leaves a compatible verdict not-a-fact', () => {
      const v = classifyPair({
        plan,
        probeDiags: [],
        poisonReason: noPoison,
        scrubCtx,
        deepFindings: {
          sent: [{ path: 'meta.owner', kind: 'any', reason: 'declared' }],
          expected: [],
        },
      });
      // The bucket is deliberately unchanged: no verdict moves because of R1d.
      assert.strictEqual(v.bucket, 'compatible');
      assert.strictEqual(v.resolved, false);
      assert.match(v.unresolved_reason!, /producer/);
      assert.match(v.unresolved_reason!, /meta\.owner/);
    });

    it('a walk that could not run is not evidence of cleanliness', () => {
      const v = classifyPair({ plan, probeDiags: [], poisonReason: noPoison, scrubCtx });
      assert.strictEqual(v.bucket, 'compatible');
      assert.strictEqual(v.resolved, false);
      assert.match(v.unresolved_reason!, /not established as a fact/);
    });

    it('a gated verdict was never a fact and says why', () => {
      const anyLine = [...plan.gateLines].find(([, n]) => n === 'sent:any')![0];
      const v = classifyPair({
        plan,
        probeDiags: [diag(anyLine, 2344)],
        poisonReason: noPoison,
        scrubCtx,
        deepFindings: clean,
      });
      assert.strictEqual(v.resolved, false);
      assert.match(v.unresolved_reason!, /'any'/);
    });
  });

  it('assignment-class error -> incompatible with real text', () => {
    const v = classifyPair({
      plan,
      probeDiags: [
        diag(plan.assignmentLine, 2741, "Property 'b' is missing in type 'X'."),
      ],
      poisonReason: noPoison,
      scrubCtx,
    });
    assert.strictEqual(v.bucket, 'incompatible');
    assert.match(v.diagnostic!, /Property 'b' is missing/);
    assert.deepStrictEqual(v.codes, [2741]);
  });

  it('IsAny gate (TS2344) -> gate_caught_baked_any on the right side', () => {
    const anyLine = [...plan.gateLines].find(([, n]) => n === 'sent:any')![0];
    const v = classifyPair({
      plan,
      probeDiags: [diag(anyLine, 2344)],
      poisonReason: noPoison,
      scrubCtx,
    });
    assert.strictEqual(v.bucket, 'gate_caught_baked_any');
    // http/response => sent is the producer.
    assert.strictEqual(v.gate, 'producer:any');
  });

  it('IsUnknown gate -> unverifiable; gate wins over a co-occurring assignment error', () => {
    const unkLine = [...plan.gateLines].find(([, n]) => n === 'sent:unknown')![0];
    const v = classifyPair({
      plan,
      // unknown produces BOTH a gate TS2344 and an assignment TS2322.
      probeDiags: [diag(unkLine, 2344), diag(plan.assignmentLine, 2322)],
      poisonReason: noPoison,
      scrubCtx,
    });
    assert.strictEqual(v.bucket, 'unverifiable');
    assert.strictEqual(v.gate, 'producer:unknown');
  });

  it('surface import error -> unverifiable (missing/renamed export)', () => {
    const v = classifyPair({
      plan,
      probeDiags: [diag(plan.importLines[0], 2305, "Module has no exported member.")],
      poisonReason: noPoison,
      scrubCtx,
    });
    assert.strictEqual(v.bucket, 'unverifiable');
    assert.strictEqual(v.gate, 'import:producer');
  });

  it('stub poison for THIS alias outranks everything -> unverifiable', () => {
    const v = classifyPair({
      plan,
      probeDiags: [diag(plan.assignmentLine, 2741)],
      // #438 part 2: poison is scoped to a (service, alias); the producer
      // alias here is poisoned, so it wins over the assignment error.
      poisonReason: (svc, alias) =>
        svc === 'orders' && alias === 'Endpoint_a_Response' ? 'conflict' : undefined,
      scrubCtx,
    });
    assert.strictEqual(v.bucket, 'unverifiable');
    assert.strictEqual(v.gate, 'poison:producer');
  });

  it('poison of a SIBLING alias in the same service does not contain this pair', () => {
    // Only a different alias of the producer service is poisoned; this pair's
    // own aliases are clean, so it must still verdict (the #438 containment).
    const v = classifyPair({
      plan,
      probeDiags: [],
      poisonReason: (svc, alias) =>
        svc === 'orders' && alias === 'Some_Other_Alias' ? 'conflict' : undefined,
      scrubCtx,
    });
    assert.strictEqual(v.bucket, 'compatible');
  });
});

describe('tsc output parsing', () => {
  it('parses primary lines and folds indented elaboration', () => {
    const out = [
      "packages/carrick-probes/probes/pair_x.ts(15,7): error TS2322: Type 'A' is not assignable to type 'B'.",
      '  Types of property "p" are incompatible.',
      'Found 1 error.',
    ].join('\n');
    const diags = parseTscOutput(out);
    assert.strictEqual(diags.length, 1);
    assert.strictEqual(diags[0].code, 2322);
    assert.strictEqual(diags[0].line, 15);
    assert.match(diags[0].message, /Types of property/);
  });
});

describe('diagnostic scrub', () => {
  const ctx = {
    workspaceRoot: '/tmp/carrick-check-v2-abc123',
    packageLabelOf: (dir: string) =>
      dir === 'orders' ? '@carrick/orders' : dir === 'web' ? '@carrick/web' : undefined,
  };

  it('maps stub-absolute import paths to @carrick/<service> labels', () => {
    const raw =
      'Type \'import("/tmp/carrick-check-v2-abc123/packages/orders/types/surface").Money\' ' +
      'is not assignable to type \'import("/tmp/carrick-check-v2-abc123/packages/web/types/surface").Money\'.';
    const out = scrubPaths(raw, ctx);
    assert.match(out, /import\("@carrick\/orders"\)/);
    assert.match(out, /import\("@carrick\/web"\)/);
    assert.ok(!out.includes('/tmp/'), out);
  });

  it('maps pnpm store paths to pkg@version', () => {
    const raw =
      'import("/tmp/carrick-check-v2-abc123/node_modules/.pnpm/bson@6.10.1/node_modules/bson/bson").ObjectId';
    const out = scrubPaths(raw, ctx);
    assert.match(out, /import\("bson@6\.10\.1"\)/);
    assert.ok(!out.includes('/tmp/'), out);
  });

  it('rewrites the Sent/Expected probe aliases to real names', () => {
    const raw = "Type 'Sent' is not assignable to type 'Expected'.";
    const out = rewriteAliases(raw, 'Endpoint_a_Response', 'Call_a_Response');
    assert.strictEqual(
      out,
      "Type 'Endpoint_a_Response' is not assignable to type 'Call_a_Response'."
    );
  });
});

describe('semver dedupe overrides', () => {
  it('collapses same-major drift to the max version', () => {
    const overrides = computeDedupeOverrides([
      { dependencies: { bson: '6.8.0' } },
      { dependencies: { bson: '6.10.1' } },
    ]);
    assert.deepStrictEqual(overrides, { 'bson@6.8.0': '6.10.1' });
  });

  it('leaves conflicting majors physically separate (no override)', () => {
    const overrides = computeDedupeOverrides([
      { dependencies: { bson: '6.10.1' } },
      { dependencies: { bson: '5.4.0' } },
    ]);
    assert.deepStrictEqual(overrides, {});
  });

  it('treats 0.x as minor-scoped compat groups', () => {
    const overrides = computeDedupeOverrides([
      { dependencies: { pkg: '0.2.1' } },
      { dependencies: { pkg: '0.2.5' } },
      { dependencies: { pkg: '0.3.0' } },
    ]);
    // 0.2.x dedupes to 0.2.5; 0.3.0 stays separate.
    assert.deepStrictEqual(overrides, { 'pkg@0.2.1': '0.2.5' });
  });

  it('identical pins need no override', () => {
    const overrides = computeDedupeOverrides([
      { dependencies: { zod: '3.23.0' } },
      { dependencies: { zod: '3.23.0' } },
    ]);
    assert.deepStrictEqual(overrides, {});
  });

  it('assembled workspace installs deterministically (offline-first, pinned resolver)', () => {
    // The scratch workspace has no committed lockfile; without these settings
    // the transitive closure is a function of live registry state and the
    // byte-stability guarantee does not hold (adversarial-review finding 6).
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-npmrc-test-'));
    const stubDir = path.join(root, 'stub');
    fs.mkdirSync(stubDir, { recursive: true });
    fs.writeFileSync(
      path.join(stubDir, 'package.json'),
      JSON.stringify({ name: '@carrick/svc', version: '0.0.0-carrick', private: true })
    );
    const ws = assembleWorkspace({
      stubs: [{ service_name: 'svc', stub_dir: stubDir }],
      workspaceRoot: root,
    });
    const npmrc = fs.readFileSync(path.join(ws.workspaceDir, '.npmrc'), 'utf8');
    assert.ok(npmrc.includes('prefer-offline=true'), npmrc);
    assert.ok(npmrc.includes('resolution-mode=highest'), npmrc);
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('0.0.x pins are each their own breaking boundary (no override)', () => {
    // Semver: every 0.0.x patch is breaking. Collapsing 0.0.3 onto 0.0.5
    // would force one physical copy and manufacture a false-compatible.
    const overrides = computeDedupeOverrides([
      { dependencies: { pkg: '0.0.3' } },
      { dependencies: { pkg: '0.0.5' } },
    ]);
    assert.deepStrictEqual(overrides, {});
  });
});
