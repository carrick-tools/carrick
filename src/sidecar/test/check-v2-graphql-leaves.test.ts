/**
 * v2 check — a GraphQL leaf is compared in the type its scalar sends
 * (carrick#4403).
 *
 * A GraphQL server sends each leaf through its scalar's `serialize`. The
 * built-in scalars are fixed by the spec: `String` and `ID` send a string,
 * `Int` and `Float` a number, `Boolean` a boolean. #4275 compared every leaf
 * in its JSON form, which is right for a custom scalar (its `serialize` is
 * not read) and wrong for a built-in one: a resolver's `bigint` under a
 * `String` leaf reaches the client as a string, and a `Date` under a `Float`
 * leaf as a number.
 *
 * The engine carries the producer service's SDL types (`graphql_types`, one
 * table per service) and the root field each response answers
 * (`graphql_root`). With both, a built-in leaf takes its scalar's type and
 * keeps the producer's own `null`/`undefined`; a custom scalar, an enum and a
 * field the SDL does not name keep the JSON form. Without them the probe is
 * #4275's, byte for byte.
 *
 * Real vendored pnpm + tsc, in the style of check-v2-graphql-wire.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import { createHash } from 'node:crypto';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runCheck, wireFormFor } from '../src/capture/index.js';
import { buildProbe } from '../src/capture/check-probe.js';
import { graphqlFieldLeaf, graphqlReachableFields } from '../src/capture/graphql-leaves.js';
import type {
  CheckPairSpec,
  CheckStubInput,
  CheckVerdict,
  GraphqlLeaf,
  GraphqlTypes,
} from '../src/capture/api.js';
import { CheckV2RequestSchema } from '../src/validators.js';

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

function gql(
  pair_key: string,
  producerAlias: string,
  consumerAlias: string,
  field?: string,
  producer = 'billing'
): CheckPairSpec {
  return {
    pair_key,
    protocol: 'graphql',
    type_kind: 'response',
    producer: { service_name: producer, alias: producerAlias },
    consumer: { service_name: 'desk', alias: consumerAlias },
    ...(field === undefined ? {} : { graphql_root: { type: 'Query', field } }),
  };
}

const nn = (leaf: GraphqlLeaf): GraphqlLeaf => ({ non_null: leaf });
const scalar = (name: string): GraphqlLeaf => ({ scalar: name });
const ref = (name: string): GraphqlLeaf => ({ type: name });
const list = (leaf: GraphqlLeaf): GraphqlLeaf => ({ list: leaf });

/** The billing service's SDL output types, one table for every root field. */
const BILLING_TYPES: GraphqlTypes = {
  Query: {
    fields: {
      receipt: ref('Receipt'),
      reading: nn(ref('Reading')),
      stamp: ref('Stamp'),
      receiptEnvelope: ref('ReceiptEnvelope'),
      batch: ref('Batch'),
      order: ref('Order'),
      loose: ref('Loose'),
      flagged: ref('Flagged'),
      opt: ref('Opt'),
      split: ref('Split'),
      agreed: ref('Agreed'),
      node: ref('Node'),
    },
  },
  Receipt: { fields: { id: nn(scalar('ID')), totalMinor: scalar('String'), at: nn(scalar('DateTime')) } },
  Reading: { fields: { id: nn(scalar('ID')), takenAt: nn(scalar('Float')) } },
  Stamp: { fields: { id: nn(scalar('ID')), at: nn(scalar('DateTime')) } },
  ReceiptEnvelope: { fields: { receipt: ref('Receipt'), warnings: nn(list(nn(scalar('String')))) } },
  Batch: { fields: { codes: nn(list(nn(scalar('String')))), rows: nn(list(ref('Row'))) } },
  Row: { fields: { amountMinor: nn(scalar('String')) } },
  Order: { fields: { id: nn(scalar('ID')), buyer: ref('Buyer') } },
  Buyer: { fields: { since: nn(scalar('Int')), name: nn(scalar('String')) } },
  Loose: { fields: { id: nn(scalar('ID')), note: scalar('String') } },
  Flagged: { fields: { state: nn({ enum: 'State' }) } },
  Opt: { fields: { totalMinor: scalar('String') } },
  // A union answers only for a field every member declares alike.
  Split: { members: ['Cash', 'Card'] },
  Agreed: { members: ['Cash', 'Voucher'] },
  Cash: { fields: { id: nn(scalar('ID')), amountMinor: nn(scalar('String')) } },
  Card: { fields: { id: nn(scalar('ID')), amountMinor: nn(scalar('Int')) } },
  Voucher: { fields: { id: nn(scalar('ID')), amountMinor: nn(scalar('String')) } },
  // A type that refers to itself.
  Node: { fields: { parent: ref('Node'), at: nn(scalar('Float')) } },
};

const PAIRS: CheckPairSpec[] = [
  // (a) bigint under String, Date under a custom scalar, against strings.
  gql('leaf-a', 'Receipt_Sent', 'Receipt_Exp', 'receipt'),
  // (a) control: the same pair with no root reads as #4275 did.
  gql('leaf-a-none', 'Receipt_Sent', 'Receipt_Exp'),
  // (b) Date under Float, against number.
  gql('leaf-b', 'Reading_Sent', 'Reading_Num_Exp', 'reading'),
  // (c) Date under Float, against string: the break JSON hid.
  gql('leaf-c', 'Reading_Sent', 'Reading_Str_Exp', 'reading'),
  // (c) controls: no root, a root the table lacks, a service with no table.
  gql('leaf-c-none', 'Reading_Sent', 'Reading_Str_Exp'),
  gql('leaf-c-unknown-root', 'Reading_Sent', 'Reading_Str_Exp', 'nowhere'),
  gql('leaf-c-no-table', 'Reading_Sent', 'Reading_Str_Exp', 'reading', 'archive'),
  // (e) a custom scalar holding a Date keeps the JSON form.
  gql('leaf-e', 'Stamp_Sent', 'Stamp_Exp', 'stamp'),
  // (f) a single-payload envelope the SDL declares: the leaves apply after
  // the unwrap, read from the envelope's field.
  gql('leaf-f', 'ReceiptEnv_Sent', 'Receipt_Exp', 'receiptEnvelope'),
  // (f) an envelope the SDL does not declare: the root leaf is the payload's.
  gql('leaf-f-payload', 'ReceiptEnv_Sent', 'Receipt_Exp', 'receipt'),
  // A list of scalars and a list of nullable objects map element by element.
  gql('leaf-lists', 'Batch_Sent', 'Batch_Exp', 'batch'),
  // A scalar list read as a single string is still a break.
  gql('leaf-list-flat', 'Batch_Sent', 'Batch_Flat_Exp', 'batch'),
  // A nested object under its own type, keeping its null.
  gql('leaf-nested', 'Order_Sent', 'Order_Exp', 'order'),
  // A deep `any` under a built-in leaf is not laundered into its scalar.
  gql('leaf-any', 'Loose_Sent', 'Loose_Exp', 'loose'),
  gql('leaf-any-none', 'Loose_Sent', 'Loose_Exp'),
  // An enum leaf keeps the JSON form; an optional field keeps its undefined.
  gql('leaf-enum', 'Flagged_Sent', 'Flagged_Exp', 'flagged'),
  gql('leaf-optional', 'Opt_Sent', 'Opt_Exp', 'opt'),
  // A union: a field its members declare differently keeps the JSON form,
  // one they declare alike takes its scalar.
  gql('leaf-union-split', 'Tender_Sent', 'Tender_Exp', 'split'),
  gql('leaf-union-agreed', 'Tender_Sent', 'Tender_Exp', 'agreed'),
  // A self-referring type ends.
  gql('leaf-cycle', 'Node_Sent', 'Node_Exp', 'node'),
];

function byKey(verdicts: CheckVerdict[]): Map<string, CheckVerdict> {
  return new Map(verdicts.map((v) => [v.pair_key, v]));
}

describe('check_v2 compares a GraphQL leaf in its scalar\'s sent type (carrick#4403)', () => {
  let verdicts: Map<string, CheckVerdict>;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-check-v2-gql-leaves-'));
    writeStub(
      root,
      'billing',
      [
        'export type Receipt_Sent = { id: string; totalMinor: bigint; at: Date };',
        'export type Reading_Sent = { id: string; takenAt: Date };',
        'export type Stamp_Sent = { id: string; at: Date };',
        'export type ReceiptEnv_Sent = { receipt: { id: string; totalMinor: bigint; at: Date }; warnings: string[] };',
        'export type Batch_Sent = { codes: bigint[]; rows: ({ amountMinor: bigint } | null)[] };',
        'export type Order_Sent = { id: string; buyer: { since: Date; name: bigint } | null };',
        'export type Loose_Sent = { id: string; note: any };',
        'export type Flagged_Sent = { state: "OPEN" | "SHUT" };',
        'export type Opt_Sent = { totalMinor?: bigint };',
        'export type Tender_Sent = { id: string; amountMinor: bigint };',
        'export interface Node_Sent { parent: Node_Sent | null; at: Date }',
      ].join('\n') + '\n'
    );
    writeStub(root, 'archive', 'export type Reading_Sent = { id: string; takenAt: Date };\n');
    writeStub(
      root,
      'desk',
      [
        'export type Receipt_Exp = { id: string; totalMinor: string | null; at: string };',
        'export type Reading_Num_Exp = { id: string; takenAt: number };',
        'export type Reading_Str_Exp = { id: string; takenAt: string };',
        'export type Stamp_Exp = { id: string; at: string };',
        'export type Batch_Exp = { codes: string[]; rows: ({ amountMinor: string } | null)[] };',
        'export type Batch_Flat_Exp = { codes: string; rows: ({ amountMinor: string } | null)[] };',
        'export type Order_Exp = { id: string; buyer: { since: number; name: string } | null };',
        'export type Loose_Exp = { id: string; note: number };',
        'export type Flagged_Exp = { state: "OPEN" | "SHUT" };',
        'export type Opt_Exp = { totalMinor?: string };',
        'export type Tender_Exp = { id: string; amountMinor: string };',
        'export interface Node_Exp { parent: Node_Exp | null; at: number }',
      ].join('\n') + '\n'
    );
    stubs = [
      { service_name: 'billing', stub_dir: path.join(root, 'billing'), graphql_types: BILLING_TYPES },
      { service_name: 'archive', stub_dir: path.join(root, 'archive') },
      { service_name: 'desk', stub_dir: path.join(root, 'desk') },
    ];
    const result = await runCheck({ stubs, pairs: PAIRS });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    assert.strictEqual(result.install_ok, true);
    verdicts = byKey(result.verdicts);
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  const bucket = (key: string) => {
    const v = verdicts.get(key);
    assert.ok(v, `verdict for ${key}`);
    return v;
  };

  it('(a) a bigint under String and a Date under a custom scalar read as strings: compatible', () => {
    const v = bucket('leaf-a');
    assert.strictEqual(v.bucket, 'compatible', JSON.stringify(v));
    assert.strictEqual(v.diagnostic, undefined);
    // Without a root the bigint stays a break, as #4275 read it.
    assert.strictEqual(bucket('leaf-a-none').bucket, 'incompatible');
  });

  it('(b) a Date under a Float leaf is sent as a number: compatible against number', () => {
    const v = bucket('leaf-b');
    assert.strictEqual(v.bucket, 'compatible', JSON.stringify(v));
  });

  it('(c) a Date under a Float leaf read as a string is a break, named without probe-local names', () => {
    const v = bucket('leaf-c');
    assert.strictEqual(v.bucket, 'incompatible', JSON.stringify(v));
    assert.ok(v.diagnostic!.includes('takenAt'), v.diagnostic);
    assert.doesNotMatch(
      v.diagnostic!,
      /GqlWire|GqlLeafWire|GqlLeafScalar|GqlScalarSent|GqlLeaves|JsonWire|GqlComparand|GqlExpected|sentWire/,
      v.diagnostic
    );
    // The accepted move (READY-4401-4404 R2): #4275 read it compatible, and
    // still does with no root, a root the table lacks, or no table.
    assert.strictEqual(bucket('leaf-c-none').bucket, 'compatible');
    assert.strictEqual(bucket('leaf-c-unknown-root').bucket, 'compatible');
    assert.strictEqual(bucket('leaf-c-no-table').bucket, 'compatible');
  });

  it('(e) a custom scalar holding a Date keeps the JSON form: compatible against string', () => {
    assert.strictEqual(bucket('leaf-e').bucket, 'compatible', JSON.stringify(bucket('leaf-e')));
  });

  it('(f) inside a single-payload envelope the leaves apply after the unwrap', () => {
    assert.strictEqual(bucket('leaf-f').bucket, 'compatible', JSON.stringify(bucket('leaf-f')));
    assert.strictEqual(
      bucket('leaf-f-payload').bucket,
      'compatible',
      JSON.stringify(bucket('leaf-f-payload'))
    );
  });

  it('a list of scalars and a list of nullable objects map element by element', () => {
    assert.strictEqual(bucket('leaf-lists').bucket, 'compatible', JSON.stringify(bucket('leaf-lists')));
    assert.strictEqual(bucket('leaf-list-flat').bucket, 'incompatible');
  });

  it('a nested object maps through its own tree, keeping its null', () => {
    assert.strictEqual(bucket('leaf-nested').bucket, 'compatible', JSON.stringify(bucket('leaf-nested')));
  });

  it('a union field the members declare alike takes its scalar; one they declare apart keeps JSON', () => {
    assert.strictEqual(bucket('leaf-union-split').bucket, 'incompatible');
    assert.strictEqual(
      bucket('leaf-union-agreed').bucket,
      'compatible',
      JSON.stringify(bucket('leaf-union-agreed'))
    );
  });

  it('a self-referring type is walked and ends', () => {
    assert.strictEqual(bucket('leaf-cycle').bucket, 'compatible', JSON.stringify(bucket('leaf-cycle')));
  });

  it('a deep any under a built-in leaf keeps the verdict it had without leaves', () => {
    assert.strictEqual(bucket('leaf-any').bucket, bucket('leaf-any-none').bucket);
  });

  it('an enum leaf and an optional field keep their form', () => {
    assert.strictEqual(bucket('leaf-enum').bucket, 'compatible');
    assert.strictEqual(bucket('leaf-optional').bucket, 'compatible', JSON.stringify(bucket('leaf-optional')));
  });
});

describe('wireFormFor reads the GraphQL types (carrick#4403)', () => {
  const graphql = { types: BILLING_TYPES, root: { type: 'Query', field: 'receipt' } };

  it('a GraphQL producer response whose root the table holds is compared in its scalars\' sent types', () => {
    assert.strictEqual(wireFormFor('graphql', 'response', 'producer', { graphql }), 'graphql_scalars');
  });

  it('without a table and a root it is the JSON form, and the facts change no other side or protocol', () => {
    assert.strictEqual(wireFormFor('graphql', 'response', 'producer'), 'json');
    assert.strictEqual(
      wireFormFor('graphql', 'response', 'producer', {
        graphql: { types: BILLING_TYPES, root: { type: 'Query', field: 'nowhere' } },
      }),
      'json'
    );
    assert.strictEqual(wireFormFor('graphql', 'response', 'consumer', { graphql }), 'declared');
    assert.strictEqual(wireFormFor('graphql', 'request', 'consumer', { graphql }), 'declared');
    assert.strictEqual(wireFormFor('http', 'response', 'producer', { graphql }), 'json');
    assert.strictEqual(wireFormFor('socket', 'both', 'consumer', { graphql }), 'declared');
  });
});

describe('the one lookup over the type table (carrick#4403)', () => {
  it('reads a field, a union\'s shared field, and nothing for a field the table lacks', () => {
    assert.deepStrictEqual(graphqlFieldLeaf(BILLING_TYPES, 'Reading', 'takenAt'), nn(scalar('Float')));
    assert.deepStrictEqual(graphqlFieldLeaf(BILLING_TYPES, 'Agreed', 'amountMinor'), nn(scalar('String')));
    assert.strictEqual(graphqlFieldLeaf(BILLING_TYPES, 'Split', 'amountMinor'), undefined);
    assert.deepStrictEqual(graphqlFieldLeaf(BILLING_TYPES, 'Split', 'id'), nn(scalar('ID')));
    assert.strictEqual(graphqlFieldLeaf(BILLING_TYPES, 'Reading', 'nope'), undefined);
    assert.strictEqual(graphqlFieldLeaf(BILLING_TYPES, 'Nope', 'id'), undefined);
  });

  it('a union that names itself answers nothing and ends', () => {
    const looped: GraphqlTypes = { Loop: { members: ['Loop', 'Cash'] }, Cash: BILLING_TYPES.Cash };
    assert.strictEqual(graphqlFieldLeaf(looped, 'Loop', 'id'), undefined);
  });

  it('the probe table holds only the types reachable from the root, in name order', () => {
    assert.deepStrictEqual(Object.keys(graphqlReachableFields(BILLING_TYPES, ref('Order'))), [
      'Buyer',
      'Order',
    ]);
    assert.deepStrictEqual(Object.keys(graphqlReachableFields(BILLING_TYPES, ref('Node'))), ['Node']);
    assert.deepStrictEqual(graphqlReachableFields(BILLING_TYPES, ref('Split')), {
      Split: { id: nn(scalar('ID')) },
    });
  });
});

describe('the check request keeps the GraphQL types (carrick#4403)', () => {
  it('the schema passes the table and the root through', () => {
    const parsed = CheckV2RequestSchema.parse({
      request_id: 'r',
      action: 'check_v2',
      stubs: [{ service_name: 'billing', stub_dir: '/x', graphql_types: BILLING_TYPES }],
      pairs: [gql('k', 'P', 'C', 'receipt')],
    });
    assert.deepStrictEqual(parsed.stubs[0].graphql_types, BILLING_TYPES);
    assert.deepStrictEqual(parsed.pairs[0].graphql_root, { type: 'Query', field: 'receipt' });
  });
});

describe('the GraphQL types leave every other probe byte for byte (carrick#4403)', () => {
  const sha = (spec: Partial<CheckPairSpec>) =>
    createHash('sha256')
      .update(
        buildProbe(
          {
            pair_key: 'k',
            protocol: 'graphql',
            type_kind: 'response',
            producer: { service_name: 'api', alias: 'P' },
            consumer: { service_name: 'web', alias: 'C' },
            ...spec,
          } as CheckPairSpec,
          (s) => `@carrick/${s}`,
          undefined,
          undefined,
          () => BILLING_TYPES
        ).source
      )
      .digest('hex');

  it('(d) a GraphQL response probe with no root is #4275\'s', () => {
    assert.strictEqual(sha({}), '10580dce23ced02ba5784e8b61c0b392aa03729cffcf1def0309d957a8d6fd8f');
  });

  it('(d) a root with no table is #4275\'s too', () => {
    assert.strictEqual(
      sha({ graphql_root: { type: 'Query', field: 'receipt' } }),
      '10580dce23ced02ba5784e8b61c0b392aa03729cffcf1def0309d957a8d6fd8f'
    );
  });

  it('http, GraphQL request, socket and pub/sub probes ignore a root', () => {
    const leaves = { graphql_root: { type: 'Query', field: 'receipt' } };
    assert.strictEqual(
      sha({ protocol: 'http', ...leaves }),
      '3d43a49784e9b26f445d69911f5a0447c405266f5b9346b8b5bd2ffd11805042'
    );
    assert.strictEqual(
      sha({ protocol: 'http', type_kind: 'request', ...leaves }),
      '6e3fc379ad433fc974c3f5b895d0fe670856ed3dd470f01833a35468dd171d89'
    );
    assert.strictEqual(
      sha({ type_kind: 'request', ...leaves }),
      '11019005ce88ad2fe800a45170885754f3cca863fc3f972aadde83f65e8e2447'
    );
    assert.strictEqual(
      sha({ protocol: 'socket', ...leaves }),
      '5a1661ba37d277a5a0ce8fd4210d0dd1a331dc7a853ddfb72ad94127ceb70fcc'
    );
  });
});
