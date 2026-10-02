/**
 * v2 check — GraphQL resolver-return envelope unwrap (end-to-end, real
 * vendored pnpm + tsc).
 *
 * A GraphQL producer's captured type is the resolver's RETURN type with
 * transport layers already peeled (`Promise<ApiResponse<Order>>` ->
 * `{ data: Order; errors: string[] }`), while the consumer's expectation is
 * the SDL field payload its selection set reads (`OrderView`). Comparing the
 * envelope raw was the corpus-1 `graphql|query|order` false-incompatible
 * ("missing the following properties from type 'OrderView': id, total"); the
 * probe now unwraps an unambiguous single-payload envelope for graphql pairs
 * (v1 ts_check `unwrapGraphqlPayload` parity, ported type-level).
 *
 * Pins the fix AND its fail-closed edges: a real field mismatch under the
 * same envelope stays incompatible, a bare payload with an optional-vs-
 * required widening (the corpus-1 subscription shape) stays incompatible
 * with its field-level diagnostic, an ambiguous envelope is never unwrapped,
 * and the unwrap is graphql-scoped (the same envelope under http keeps its
 * raw comparison).
 *
 * The same pairs pin the `__typename` rule (carrick#1759). A consumer type
 * generated from a document that selects `__typename` declares it required,
 * and a resolver's return type never states it, because the GraphQL server
 * adds that meta-field to every object a selection asks for. So a GraphQL
 * consumer's `__typename` is read as optional, at any depth: a producer that
 * omits it satisfies the consumer, a producer that states a different one
 * still does not, a real field mismatch beside it stays incompatible and is
 * named without it, and an http pair keeps `__typename` as an ordinary field.
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

function gql(pair_key: string, producerAlias: string, consumerAlias: string): CheckPairSpec {
  return {
    pair_key,
    protocol: 'graphql',
    type_kind: 'response',
    producer: { service_name: 'gateway', alias: producerAlias },
    consumer: { service_name: 'web', alias: consumerAlias },
  };
}

const PAIRS: CheckPairSpec[] = [
  // The corpus-1 `graphql|query|order` shape: envelope producer, subset consumer.
  gql('gql-envelope-subset', 'Env_Subset_Sent', 'Env_Subset_Exp'),
  // Inverse: a REAL field-type mismatch under the same envelope.
  gql('gql-envelope-mismatch', 'Env_Mismatch_Sent', 'Env_Mismatch_Exp'),
  // Bare payload, consumer selects a subset: plain assignability, no unwrap needed.
  gql('gql-bare-subset', 'Bare_Subset_Sent', 'Bare_Subset_Exp'),
  // The corpus-1 `graphql|subscription|orderUpdated` shape: bare payload with
  // an object-typed prop AND a union-of-objects prop; optional `note` vs
  // required. The unwrap must NOT misfire onto the single object prop.
  gql('gql-bare-widening', 'Bare_Widening_Sent', 'Bare_Widening_Exp'),
  // Two payload-shaped properties: ambiguous envelope, never unwrapped.
  gql('gql-envelope-ambiguous', 'Env_Ambiguous_Sent', 'Env_Ambiguous_Exp'),
  // Protocol scope: the same envelope shape under http keeps the raw compare.
  {
    pair_key: 'http-envelope-scoped',
    protocol: 'http',
    type_kind: 'response',
    producer: { service_name: 'gateway', alias: 'Env_Http_Sent' },
    consumer: { service_name: 'web', alias: 'Env_Http_Exp' },
  },
  // carrick#1759: a generated fragment type requires `__typename`; the
  // resolver's row type never states it.
  gql('gql-typename-root', 'Tn_Root_Sent', 'Tn_Root_Exp'),
  // `__typename` inside arrays, a nullable object and a tuple member.
  gql('gql-typename-nested', 'Tn_Nested_Sent', 'Tn_Nested_Exp'),
  // A connection-shaped selection, `__typename` eleven levels down.
  gql('gql-typename-deep', 'Tn_Deep_Sent', 'Tn_Deep_Exp'),
  // A bare payload with ONE object-shaped property: the envelope short-circuit
  // must test the relaxed consumer, or the unwrap fires on that property.
  gql('gql-typename-bare-one-object', 'Tn_BareOne_Sent', 'Tn_BareOne_Exp'),
  // A real envelope still unwraps when the consumer requires `__typename`.
  gql('gql-typename-envelope', 'Tn_Env_Sent', 'Tn_Env_Exp'),
  // The producer states `__typename` at the root and not below it.
  gql('gql-typename-stated', 'Tn_Stated_Sent', 'Tn_Stated_Exp'),
  // A real field mismatch beside the meta-field stays incompatible.
  gql('gql-typename-real-mismatch', 'Tn_Mismatch_Sent', 'Tn_Mismatch_Exp'),
  // The same, from a producer that states `__typename` itself.
  gql('gql-typename-stated-mismatch', 'Tn_StatedMismatch_Sent', 'Tn_StatedMismatch_Exp'),
  // Past the probe's depth bound the consumer is compared as declared.
  gql('gql-typename-beyond-bound', 'Tn_Bound_Sent', 'Tn_Bound_Exp'),
  // A producer that states a different `__typename` is not satisfied by it.
  gql('gql-typename-conflict', 'Tn_Conflict_Sent', 'Tn_Conflict_Exp'),
  // A function-typed member is compared as declared, not mapped into an object.
  gql('gql-typename-function-member', 'Tn_Fn_Sent', 'Tn_Fn_Exp'),
  // The rule is graphql-scoped: under http `__typename` is an ordinary field.
  {
    pair_key: 'http-typename-scoped',
    protocol: 'http',
    type_kind: 'response',
    producer: { service_name: 'gateway', alias: 'Tn_Http_Sent' },
    consumer: { service_name: 'web', alias: 'Tn_Http_Exp' },
  },
];

/**
 * A connection-shaped selection: `viewer.team.members.edges[].node.tasks.edges[]
 * .node.assignee`, with `__typename` on the innermost object (and, on the
 * consumer, on every object). Eleven levels, counting each array as one.
 */
/** Twenty nested objects, `__typename` only on the innermost (consumer side). */
function nested20(typename: boolean): string {
  let shape = `{ ${typename ? '__typename: "Leaf"; ' : ''}id: string }`;
  for (let i = 0; i < 19; i++) shape = `{ child: ${shape} }`;
  return shape;
}

function connectionShape(typenames: boolean): string {
  const tn = (name: string) => (typenames ? `__typename: "${name}"; ` : '');
  const assignee = `{ ${tn('User')}id: string; name: string }`;
  const taskNode = `{ ${tn('Task')}id: string; assignee: ${assignee} }`;
  const taskEdges = `{ ${tn('TaskConnection')}edges: Array<{ ${tn('TaskEdge')}node: ${taskNode} }> }`;
  const memberNode = `{ ${tn('Member')}id: string; tasks: ${taskEdges} }`;
  const members = `{ ${tn('MemberConnection')}edges: Array<{ ${tn('MemberEdge')}node: ${memberNode} }> }`;
  const team = `{ ${tn('Team')}id: string; members: ${members} }`;
  return `{ ${tn('Viewer')}id: string; team: ${team} }`;
}

function byKey(verdicts: CheckVerdict[]): Map<string, CheckVerdict> {
  return new Map(verdicts.map((v) => [v.pair_key, v]));
}

describe('check_v2 graphql envelope unwrap (real pnpm + tsc)', () => {
  let verdicts: Map<string, CheckVerdict>;
  let rerun: CheckVerdict[];
  let first: CheckVerdict[];

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-check-v2-gql-'));
    writeStub(
      root,
      'gateway',
      [
        // Corpus-1 resolveOrder: Promise-unwrapped ApiResponse<Order> envelope.
        'export type Env_Subset_Sent = { data: { id: string; total: { amountCents: number; currency: string }; status: { kind: "placed"; placedAt: string } | { kind: "refunded"; refundedAt: string; reason?: string }; note?: string }; errors: string[] };',
        // Same envelope, but total.amountCents is a string: a real wire break.
        'export type Env_Mismatch_Sent = { data: { id: string; total: { amountCents: string; currency: string }; note?: string }; errors: string[] };',
        'export type Bare_Subset_Sent = { id: string; total: { amountCents: number; currency: string }; note?: string };',
        // Corpus-1 Order: object prop (total) + union-of-objects prop (status).
        'export type Bare_Widening_Sent = { id: string; total: { amountCents: number; currency: string }; status: { kind: "placed"; placedAt: string } | { kind: "refunded"; refundedAt: string }; note?: string };',
        'export type Env_Ambiguous_Sent = { data: { id: string }; meta: { traceId: string }; errors: string[] };',
        'export type Env_Http_Sent = { data: { id: string }; errors: string[] };',
        // A row type: no `__typename`, every field always sent.
        'export type Tn_Root_Sent = { id: string; name: string; email: null | string; createdAt: string };',
        'export type Tn_Nested_Sent = { id: string; lines: { id: string; qty: number }[]; owner: { name: string }; pair: [{ a: number }, string] };',
        `export type Tn_Deep_Sent = ${connectionShape(false)};`,
        'export type Tn_BareOne_Sent = { id: string; total: { amountCents: number; currency: string }; note?: string };',
        'export type Tn_Env_Sent = { data: { id: string; total: { amountCents: number; currency: string } }; errors: string[] };',
        'export type Tn_Stated_Sent = { __typename: "Order"; id: string; total: { amountCents: number } };',
        'export type Tn_Mismatch_Sent = { id: string; qty: string; createdAt: string };',
        'export type Tn_StatedMismatch_Sent = { __typename: "Line"; id: string; qty: string };',
        `export type Tn_Bound_Sent = ${nested20(false)};`,
        'export type Tn_Conflict_Sent = { __typename: "Refund"; id: string };',
        'export type Tn_Fn_Sent = { id: string; format: number };',
        'export type Tn_Http_Sent = { id: string };',
      ].join('\n') + '\n'
    );
    writeStub(
      root,
      'web',
      [
        'export type Env_Subset_Exp = { id: string; total: { amountCents: number; currency: string }; note?: string };',
        'export type Env_Mismatch_Exp = { id: string; total: { amountCents: number; currency: string }; note?: string };',
        'export type Bare_Subset_Exp = { id: string };',
        'export type Bare_Widening_Exp = { id: string; total: { amountCents: number; currency: string }; note: string };',
        'export type Env_Ambiguous_Exp = { id: string };',
        'export type Env_Http_Exp = { id: string };',
        // A generated fragment type: `__typename` required, the rest optional.
        'export type Tn_Root_Exp = { __typename: "Contact"; id?: string | null; name?: string | null; email?: string | null };',
        'export type Tn_Nested_Exp = { __typename: "Order"; id: string; lines: Array<{ __typename: "Line"; id: string; qty: number }>; owner: { __typename: "User"; name: string } | null; pair: [{ __typename: "Pair"; a: number }, string] };',
        `export type Tn_Deep_Exp = ${connectionShape(true)};`,
        'export type Tn_BareOne_Exp = { __typename: "Order"; id: string; total: { __typename: "Money"; amountCents: number; currency: string } };',
        'export type Tn_Env_Exp = { __typename: "Order"; id: string; total: { __typename: "Money"; amountCents: number } };',
        'export type Tn_Stated_Exp = { __typename: "Order"; id: string; total: { __typename: "Money"; amountCents: number } };',
        'export type Tn_Mismatch_Exp = { __typename: "Line"; id: string; qty: number };',
        'export type Tn_StatedMismatch_Exp = { __typename: "Line"; id: string; qty: number };',
        `export type Tn_Bound_Exp = ${nested20(true)};`,
        'export type Tn_Conflict_Exp = { __typename: "Order"; id: string };',
        'export type Tn_Fn_Exp = { __typename: "Order"; id: string; format: () => string };',
        'export type Tn_Http_Exp = { __typename: "Order"; id: string };',
      ].join('\n') + '\n'
    );
    stubs = [
      { service_name: 'gateway', stub_dir: path.join(root, 'gateway') },
      { service_name: 'web', stub_dir: path.join(root, 'web') },
    ];
    const result = await runCheck({ stubs, pairs: PAIRS });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    assert.strictEqual(result.install_ok, true);
    first = result.verdicts;
    verdicts = byKey(result.verdicts);
    const second = await runCheck({ stubs, pairs: PAIRS });
    assert.strictEqual(second.success, true, JSON.stringify(second.errors));
    rerun = second.verdicts;
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('envelope producer vs subset consumer -> compatible (the corpus-1 query fix)', () => {
    const v = verdicts.get('gql-envelope-subset')!;
    assert.strictEqual(v.bucket, 'compatible');
    assert.strictEqual(v.diagnostic, undefined);
  });

  it('real field mismatch under the same envelope -> incompatible, payload-level diagnostic', () => {
    const v = verdicts.get('gql-envelope-mismatch')!;
    assert.strictEqual(v.bucket, 'incompatible');
    assert.ok(v.diagnostic!.includes('amountCents'), v.diagnostic);
    // A consumer with no `__typename` to relax is compared as declared, so the
    // headline still names its surface alias.
    assert.ok(v.diagnostic!.includes("'Env_Mismatch_Exp'"), v.diagnostic);
  });

  it('bare payload subset selection stays compatible (no unwrap needed)', () => {
    assert.strictEqual(verdicts.get('gql-bare-subset')!.bucket, 'compatible');
  });

  it('optional-vs-required widening on a bare payload stays incompatible with its field diagnostic (no misfire onto the single object prop)', () => {
    const v = verdicts.get('gql-bare-widening')!;
    assert.strictEqual(v.bucket, 'incompatible');
    // The union-of-objects sibling (status) blocks single-payload selection, so
    // the diagnostic elaborates the real `note` widening, not a bogus unwrap.
    assert.ok(v.diagnostic!.includes("'note'"), v.diagnostic);
  });

  it('ambiguous envelope (two payload-shaped props) is never unwrapped -> incompatible', () => {
    assert.strictEqual(verdicts.get('gql-envelope-ambiguous')!.bucket, 'incompatible');
  });

  it('the unwrap is graphql-scoped: the same envelope under http stays incompatible', () => {
    assert.strictEqual(verdicts.get('http-envelope-scoped')!.bucket, 'incompatible');
  });

  // carrick#1759. Every compatible pair here also asserts an empty `codes`: a
  // diagnostic on one of the probe's helper `type` lines reaches no bucket,
  // so `codes` is the only place it would show.
  for (const key of [
    'gql-typename-root',
    'gql-typename-nested',
    'gql-typename-deep',
    'gql-typename-bare-one-object',
    'gql-typename-envelope',
    // The relaxed `__typename` is the probe's reading, not the consumer's
    // source: a producer that sends it gets no "optional on the consumer" note.
    'gql-typename-stated',
  ]) {
    it(`${key}: a consumer's required __typename is supplied by the server -> compatible`, () => {
      const v = verdicts.get(key)!;
      assert.strictEqual(v.bucket, 'compatible', v.diagnostic);
      assert.deepStrictEqual(v.codes, []);
      assert.strictEqual(v.diagnostic, undefined);
      for (const note of v.notes ?? []) {
        assert.ok(!note.includes('__typename'), note);
      }
    });
  }

  it('a real field mismatch beside __typename stays incompatible and names the field, not __typename', () => {
    const v = verdicts.get('gql-typename-real-mismatch')!;
    assert.strictEqual(v.bucket, 'incompatible');
    assert.ok(v.diagnostic!.includes("'qty' is string on the producer and number on the consumer"), v.diagnostic);
    // The headline prints the consumer as compared (`__typename?: "Line"`), but
    // neither tsc nor the field sentence names `__typename` as a difference.
    assert.ok(!v.diagnostic!.includes("'__typename'"), v.diagnostic);
    // An extra producer field is named only beside a field the consumer waits
    // on and never gets; the server-supplied `__typename` is not one.
    assert.ok(!v.diagnostic!.includes("'createdAt'"), v.diagnostic);
  });

  it('a producer that states __typename gets no "optional on the consumer" statement for it', () => {
    const v = verdicts.get('gql-typename-stated-mismatch')!;
    assert.strictEqual(v.bucket, 'incompatible');
    assert.ok(v.diagnostic!.includes("'qty' is string on the producer and number on the consumer"), v.diagnostic);
    assert.ok(!v.diagnostic!.includes("'__typename'"), v.diagnostic);
  });

  it('past the depth bound the consumer is compared as declared (the bound ends the expansion)', () => {
    const v = verdicts.get('gql-typename-beyond-bound')!;
    assert.strictEqual(v.bucket, 'incompatible');
    assert.ok(v.diagnostic!.includes("'Tn_Bound_Exp'"), v.diagnostic);
  });

  it('a producer that states a different __typename stays incompatible', () => {
    const v = verdicts.get('gql-typename-conflict')!;
    assert.strictEqual(v.bucket, 'incompatible');
    assert.ok(v.diagnostic!.includes('"Refund"'), v.diagnostic);
  });

  it('a function-typed consumer member is compared as declared', () => {
    const v = verdicts.get('gql-typename-function-member')!;
    assert.strictEqual(v.bucket, 'incompatible');
    assert.ok(v.diagnostic!.includes("'format'"), v.diagnostic);
  });

  it('the __typename rule is graphql-scoped: an http consumer requiring it stays incompatible', () => {
    const v = verdicts.get('http-typename-scoped')!;
    assert.strictEqual(v.bucket, 'incompatible');
    // Both the judge and the field walk read it as an ordinary required field.
    assert.ok(v.diagnostic!.includes("Property '__typename' is missing"), v.diagnostic);
    assert.ok(
      v.diagnostic!.includes("'__typename' is required by the consumer and the producer does not send it"),
      v.diagnostic
    );
  });

  it('verdicts are byte-stable across two independent runs', () => {
    assert.strictEqual(JSON.stringify(first), JSON.stringify(rerun));
  });
});
