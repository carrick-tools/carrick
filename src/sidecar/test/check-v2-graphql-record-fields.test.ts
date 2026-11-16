/**
 * v2 check — a field the served schema declares and the producer's root
 * record lacks is not a break (carrick#1875).
 *
 * A GraphQL resolver often returns a record without some fields the schema
 * declares: relations and computed fields are supplied by field resolvers the
 * check does not read. Comparing the record alone then reports every such
 * field as missing, a break that cannot happen. With the producer's SDL types
 * and the root field, the probe asks a second question when the wire
 * assignment fails: would it pass if every field the SDL declares and the
 * record lacks were filled? If so the pair reads unverifiable
 * (`producer:field_resolver`), not incompatible. A field the SDL does not
 * declare, and a real leaf mismatch beside the absent fields, stay breaks.
 * Without a table the probe is what it was, byte for byte.
 *
 * Real vendored pnpm + tsc, in the style of check-v2-graphql-leaves.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import { createHash } from 'node:crypto';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runCheck } from '../src/capture/index.js';
import { buildProbe } from '../src/capture/check-probe.js';
import type {
  CheckPairSpec,
  CheckStubInput,
  CheckVerdict,
  GraphqlLeaf,
  GraphqlTypes,
} from '../src/capture/api.js';

let root: string;

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
  field?: string
): CheckPairSpec {
  return {
    pair_key,
    protocol: 'graphql',
    type_kind: 'response',
    producer: { service_name: 'shop', alias: producerAlias },
    consumer: { service_name: 'till', alias: consumerAlias },
    ...(field === undefined ? {} : { graphql_root: { type: 'Query', field } }),
  };
}

const nn = (leaf: GraphqlLeaf): GraphqlLeaf => ({ non_null: leaf });
const scalar = (name: string): GraphqlLeaf => ({ scalar: name });
const ref = (name: string): GraphqlLeaf => ({ type: name });
const list = (leaf: GraphqlLeaf): GraphqlLeaf => ({ list: leaf });

/** The shop service's SDL output types. */
const SHOP_TYPES: GraphqlTypes = {
  Query: {
    fields: {
      order: ref('Order'),
      orders: nn(list(nn(ref('Order')))),
      node: ref('Node'),
    },
  },
  Order: {
    fields: {
      id: nn(scalar('ID')),
      total: nn(scalar('Int')),
      label: nn(scalar('String')),
      lines: nn(list(nn(ref('Line')))),
    },
  },
  Line: { fields: { sku: nn(scalar('String')) } },
  Node: { fields: { id: nn(scalar('ID')), at: nn(scalar('Float')), parent: ref('Node') } },
};

const PAIRS: CheckPairSpec[] = [
  // (a) the consumer reads `label`, which the SDL declares and the record lacks.
  gql('rf-a', 'Order_Sent', 'Label_Exp', 'order'),
  // (b) the consumer reads `missing`, which the SDL does not declare.
  gql('rf-b', 'Order_Sent', 'Missing_Exp', 'order'),
  // (c) a resolver-served field beside a real leaf mismatch on `total`.
  gql('rf-c', 'Order_Sent', 'LabelTotal_Exp', 'order'),
  // (d) a nested record lacks `sku`, which its SDL type declares.
  gql('rf-d', 'Order_Nested_Sent', 'Sku_Exp', 'order'),
  // (e) the pair of (a) with no table: compared on the record as before.
  gql('rf-e', 'Order_Sent', 'Label_Exp'),
  // (f) a pair that assigns as it is.
  gql('rf-f', 'Order_Sent', 'Id_Exp', 'order'),
  // A nullable root record and a list root, each lacking `label`.
  gql('rf-nullable', 'OrderOrNull_Sent', 'LabelOrNull_Exp', 'order'),
  gql('rf-list', 'Orders_Sent', 'LabelList_Exp', 'orders'),
  // A self-referring record read to a fixed depth, as a selection reads it:
  // an absent `id` is served; a leaf mismatch breaks.
  gql('rf-cycle', 'Node_Sent', 'NodeId_Exp', 'node'),
  gql('rf-cycle-leaf', 'Node_Sent', 'NodeBad_Exp', 'node'),
  // The same two against consumers that name themselves (no fixed depth).
  gql('rf-cycle-rec', 'Node_Sent', 'NodeIdRec_Exp', 'node'),
  gql('rf-cycle-rec-leaf', 'Node_Sent', 'NodeBadRec_Exp', 'node'),
];

function byKey(verdicts: CheckVerdict[]): Map<string, CheckVerdict> {
  return new Map(verdicts.map((v) => [v.pair_key, v]));
}

describe('check_v2: a field the served schema declares and the record lacks is unverifiable (carrick#1875)', () => {
  let verdicts: Map<string, CheckVerdict>;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-check-v2-gql-record-fields-'));
    writeStub(
      root,
      'shop',
      [
        'export type Order_Sent = { id: string; total: number; lines: { sku: string }[] };',
        'export type Order_Nested_Sent = { id: string; total: number; label: string; lines: { qty: number }[] };',
        'export type OrderOrNull_Sent = { id: string; total: number; lines: { sku: string }[] } | null;',
        'export type Orders_Sent = { id: string; total: number; lines: { sku: string }[] }[];',
        'export interface Node_Sent { at: Date; parent: Node_Sent | null }',
      ].join('\n') + '\n'
    );
    writeStub(
      root,
      'till',
      [
        'export type Label_Exp = { id: string; label: string };',
        'export type Missing_Exp = { id: string; missing: string };',
        'export type LabelTotal_Exp = { label: string; total: string };',
        'export type Sku_Exp = { id: string; lines: { sku: string }[] };',
        'export type Id_Exp = { id: string };',
        'export type LabelOrNull_Exp = { id: string; label: string } | null;',
        'export type LabelList_Exp = { id: string; label: string }[];',
        'export type NodeId_Exp = { id: string; at: number; parent: { id: string; at: number } | null };',
        'export type NodeBad_Exp = { at: string; parent: { at: string } | null };',
        'export interface NodeIdRec_Exp { id: string; at: number; parent: NodeIdRec_Exp | null }',
        'export interface NodeBadRec_Exp { at: string; parent: NodeBadRec_Exp | null }',
      ].join('\n') + '\n'
    );
    const stubs: CheckStubInput[] = [
      { service_name: 'shop', stub_dir: path.join(root, 'shop'), graphql_types: SHOP_TYPES },
      { service_name: 'till', stub_dir: path.join(root, 'till') },
    ];
    const result = await runCheck({ stubs, pairs: PAIRS });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    assert.strictEqual(result.install_ok, true);
    verdicts = byKey(result.verdicts);
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  const verdict = (key: string): CheckVerdict => {
    const v = verdicts.get(key);
    assert.ok(v, `verdict for ${key}`);
    return v;
  };

  const assertResolverServed = (key: string): void => {
    const v = verdict(key);
    assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
    assert.strictEqual(v.gate, 'producer:field_resolver', JSON.stringify(v));
    assert.strictEqual(v.resolved, false);
    assert.match(v.unresolved_reason ?? '', /field resolver/);
    // No side to blame, so no retype runs.
    assert.strictEqual(v.unresolved_side, undefined, JSON.stringify(v));
  };

  it('(a) a field the SDL declares and the record lacks: unverifiable, not a break', () => {
    assertResolverServed('rf-a');
  });

  it('(b) a field the SDL does not declare stays a break', () => {
    const v = verdict('rf-b');
    assert.strictEqual(v.bucket, 'incompatible', JSON.stringify(v));
    assert.ok(v.diagnostic!.includes('missing'), v.diagnostic);
  });

  it('(c) a real leaf mismatch beside a resolver-served field stays a break', () => {
    const v = verdict('rf-c');
    assert.strictEqual(v.bucket, 'incompatible', JSON.stringify(v));
    assert.doesNotMatch(v.diagnostic!, /GqlSdlFilled|SdlFieldDecides|GqlWire|GqlTypes/, v.diagnostic);
  });

  it('(d) a nested record lacking a field its SDL type declares: unverifiable', () => {
    assertResolverServed('rf-d');
  });

  it('(e) with no table the pair is compared on the record, as before', () => {
    assert.strictEqual(verdict('rf-e').bucket, 'incompatible', JSON.stringify(verdict('rf-e')));
  });

  it('(f) a pair that assigns as it is stays compatible', () => {
    assert.strictEqual(verdict('rf-f').bucket, 'compatible', JSON.stringify(verdict('rf-f')));
  });

  it('a nullable root record and a list root lacking a declared field: unverifiable', () => {
    assertResolverServed('rf-nullable');
    assertResolverServed('rf-list');
  });

  it('a self-referring record: an absent declared field is unverifiable, a leaf mismatch a break', () => {
    assertResolverServed('rf-cycle');
    const v = verdict('rf-cycle-leaf');
    assert.strictEqual(v.bucket, 'incompatible', JSON.stringify(v));
    assert.strictEqual(v.gate, undefined, JSON.stringify(v));
  });

  it('a consumer that names itself reads the same: unverifiable, and a leaf mismatch a break', () => {
    assertResolverServed('rf-cycle-rec');
    const v = verdict('rf-cycle-rec-leaf');
    assert.strictEqual(v.bucket, 'incompatible', JSON.stringify(v));
    assert.strictEqual(v.gate, undefined, JSON.stringify(v));
    assert.ok(!v.codes.includes(2589), JSON.stringify(v.codes));
  });
});

describe('the record-field gate leaves other probes as they were (carrick#1875)', () => {
  const probe = (field?: string, types: GraphqlTypes | null = SHOP_TYPES) =>
    buildProbe(
      gql('k', 'Order_Sent', 'Label_Exp', field),
      (s) => `@carrick/${s}`,
      undefined,
      undefined,
      () => types ?? undefined
    );
  const sha = (text: string) => createHash('sha256').update(text).digest('hex');

  it('(e) a probe with no root, or no table, is main\'s byte for byte', () => {
    const main = 'cb81d95c2c70a0635657cc64ba19ae4582f6659d9086dad937fae3582bdfa09a';
    assert.strictEqual(sha(probe().source), main);
    assert.strictEqual(sha(probe('order', null).source), main);
  });

  it('(f) every line up to the wire assignment is main\'s; the gate comes after it', () => {
    const p = probe('order');
    const lines = p.source.split('\n');
    assert.strictEqual(p.wireAssignmentLine, 47);
    assert.strictEqual(
      sha(lines.slice(0, p.wireAssignmentLine).join('\n')),
      'c2b40c5e6d1c8f376ca10abe3fb7fcf565eca21e3ea8f651cf83fd9826677560'
    );
    const gate = [...p.gateLines].find(([, name]) => (name as string) === 'sent:field_resolver');
    assert.ok(gate, 'the probe carries the field_resolver gate');
    assert.ok(gate[0] > p.wireAssignmentLine!, `gate line ${gate[0]}`);
  });
});

/**
 * Rule G (carrick#1875): the record HAS the field, typed as a generic JSON
 * value or a primitive, where the schema declares an object type (or a list
 * of them). A field resolver converts it; the check reads no resolver, so
 * the pair is unverifiable. The JSON value is recognised by its structure
 * (every JSON primitive, arrays of itself, string-keyed objects of itself),
 * spelled here the way ORM clients emit it. A record field that is an object
 * shape is still compared field by field.
 */
const YARD_TYPES: GraphqlTypes = {
  Query: { fields: { order: ref('Order') } },
  Order: {
    fields: {
      id: nn(scalar('ID')),
      warnings: nn(list(nn(ref('Warning')))),
      lead: nn(ref('Warning')),
      note: nn(scalar('String')),
      placed: nn(scalar('String')),
    },
  },
  Warning: { fields: { code: nn(scalar('String')) } },
};

const yard = (pair_key: string, producerAlias: string, consumerAlias: string): CheckPairSpec => ({
  pair_key,
  protocol: 'graphql',
  type_kind: 'response',
  producer: { service_name: 'yard', alias: producerAlias },
  consumer: { service_name: 'dock', alias: consumerAlias },
  graphql_root: { type: 'Query', field: 'order' },
});

const YARD_PAIRS: CheckPairSpec[] = [
  // (1) a JSON value where the schema declares a list of objects.
  yard('rg-json-list', 'OrderJson_Sent', 'Warnings_Exp'),
  // (1b) the same beside a Date, so the wire line rewrites the record first.
  yard('rg-json-list-wire', 'OrderJsonPlaced_Sent', 'WarningsPlaced_Exp'),
  // (2) a primitive where the schema declares an object type.
  yard('rg-string-object', 'OrderLeadString_Sent', 'Lead_Exp'),
  // (3) an object shape lacking a sub-field the schema does not declare.
  yard('rg-shape-undeclared', 'OrderShape_Sent', 'WarningsHint_Exp'),
  // (4) a JSON value where the schema declares a scalar the consumer mismatches.
  yard('rg-json-scalar', 'OrderNoteJson_Sent', 'NoteNumber_Exp'),
  // An object shape or a string: the shape is compared, so (3)'s gap breaks.
  yard('rg-shape-or-string', 'OrderLeadEither_Sent', 'LeadHint_Exp'),
  // An array of strings under a list of objects: not a JSON value, not a
  // primitive; compared as before.
  yard('rg-string-array', 'OrderStringArray_Sent', 'Warnings_Exp'),
];

describe('check_v2: a record field the schema converts to an object type is unverifiable (carrick#1875 rule G)', () => {
  let groot: string;
  let verdicts: Map<string, CheckVerdict>;

  before(async () => {
    groot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-check-v2-gql-record-convert-'));
    writeStub(
      groot,
      'yard',
      [
        'export type Blob = string | number | boolean | BlobObject | BlobArray | null;',
        'export type BlobObject = { [Key in string]?: Blob };',
        'export interface BlobArray extends Array<Blob> {}',
        'export type OrderJson_Sent = { id: string; warnings: Blob };',
        'export type OrderJsonPlaced_Sent = { id: string; placed: Date; warnings: Blob };',
        'export type OrderLeadString_Sent = { id: string; lead: string };',
        'export type OrderShape_Sent = { id: string; warnings: { code: string }[] };',
        'export type OrderNoteJson_Sent = { id: string; note: Blob };',
        'export type OrderLeadEither_Sent = { id: string; lead: { code: string } | string };',
        'export type OrderStringArray_Sent = { id: string; warnings: string[] };',
      ].join('\n') + '\n'
    );
    writeStub(
      groot,
      'dock',
      [
        'export type Warnings_Exp = { id: string; warnings: { code: string }[] };',
        'export type WarningsPlaced_Exp = { id: string; placed: string; warnings: { code: string }[] };',
        'export type Lead_Exp = { id: string; lead: { code: string } };',
        'export type WarningsHint_Exp = { id: string; warnings: { code: string; hint: string }[] };',
        'export type NoteNumber_Exp = { id: string; note: number };',
        'export type LeadHint_Exp = { id: string; lead: { code: string; hint: string } };',
      ].join('\n') + '\n'
    );
    const stubs: CheckStubInput[] = [
      { service_name: 'yard', stub_dir: path.join(groot, 'yard'), graphql_types: YARD_TYPES },
      { service_name: 'dock', stub_dir: path.join(groot, 'dock') },
    ];
    const result = await runCheck({ stubs, pairs: YARD_PAIRS });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    assert.strictEqual(result.install_ok, true);
    verdicts = byKey(result.verdicts);
  });

  after(() => {
    fs.rmSync(groot, { recursive: true, force: true });
  });

  const verdict = (key: string): CheckVerdict => {
    const v = verdicts.get(key);
    assert.ok(v, `verdict for ${key}`);
    assert.ok(!v.codes.includes(2589), JSON.stringify(v.codes));
    return v;
  };

  const assertConverted = (key: string): void => {
    const v = verdict(key);
    assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
    assert.strictEqual(v.gate, 'producer:field_resolver', JSON.stringify(v));
    assert.strictEqual(v.resolved, false);
    assert.strictEqual(v.unresolved_side, undefined, JSON.stringify(v));
  };

  const assertBreak = (key: string): void => {
    const v = verdict(key);
    assert.strictEqual(v.bucket, 'incompatible', JSON.stringify(v));
    assert.strictEqual(v.gate, undefined, JSON.stringify(v));
  };

  it('(1) a JSON value where the schema declares a list of objects: unverifiable', () => {
    assertConverted('rg-json-list');
  });

  it('(1b) the same after the wire line rewrites the record: unverifiable', () => {
    assertConverted('rg-json-list-wire');
  });

  it('(2) a primitive where the schema declares an object type: unverifiable', () => {
    assertConverted('rg-string-object');
  });

  it('(3) an object shape lacking a sub-field the schema does not declare stays a break', () => {
    assertBreak('rg-shape-undeclared');
  });

  it('(4) a JSON value under a scalar the consumer mismatches stays a break', () => {
    assertBreak('rg-json-scalar');
  });

  it('an object shape or a string is compared as a shape and stays a break', () => {
    assertBreak('rg-shape-or-string');
  });

  it('an array of strings under a list of objects stays a break', () => {
    assertBreak('rg-string-array');
  });
});
