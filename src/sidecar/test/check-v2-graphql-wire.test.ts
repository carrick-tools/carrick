/**
 * v2 check — a GraphQL producer response is compared in the form the server
 * sends (carrick#4275 T1).
 *
 * A GraphQL server sends each leaf through its scalar's `serialize`, so a
 * resolver that returns a `Date` puts a string on the wire, and a client
 * reads a string. Comparing the resolver's declared `Date` with the client's
 * `string` reported a break the wire does not have. The judge now applies the
 * JSON wire form to the producer's response, after the envelope unwrap, so
 * the unwrap and the transform never meet. A request stays declared, as do
 * socket and pub/sub payloads, and the http probe is unchanged byte for byte.
 *
 * Real vendored pnpm + tsc, in the style of check-v2-graphql-envelope.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import { createHash } from 'node:crypto';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runCheck } from '../src/capture/index.js';
import { buildProbe } from '../src/capture/check-probe.js';
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
    producer: { service_name: 'ledger', alias: producerAlias },
    consumer: { service_name: 'portal', alias: consumerAlias },
  };
}

const PAIRS: CheckPairSpec[] = [
  // (a) A resolver row with a Date, read by a client as a string.
  gql('gql-wire-date-string', 'Bill_Sent', 'Bill_Str_Exp'),
  // (b) The same row inside a single-payload envelope.
  gql('gql-wire-envelope', 'BillEnv_Sent', 'BillEnv_Exp'),
  // (c) A client that reads the date as a number: still a break.
  gql('gql-wire-number', 'Bill_Sent', 'Bill_Num_Exp'),
  // (d) A client that declares the Date as well: agrees as declared.
  gql('gql-wire-declared', 'Bill_Sent', 'Bill_Date_Exp'),
  // (e) A bigint member against a string: JSON cannot send a bigint.
  gql('gql-wire-bigint', 'Tally_Sent', 'Tally_Exp'),
];

function byKey(verdicts: CheckVerdict[]): Map<string, CheckVerdict> {
  return new Map(verdicts.map((v) => [v.pair_key, v]));
}

const WIRE_NOTE =
  "The producer's type is compared in the form JSON puts on the wire: a value with a toJSON() method (a Date, for example) travels as what it serialises to.";

describe('check_v2 compares a GraphQL producer response in its wire form (carrick#4275)', () => {
  let verdicts: Map<string, CheckVerdict>;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-check-v2-gql-wire-'));
    writeStub(
      root,
      'ledger',
      [
        'export type Bill_Sent = { id: string; issuedOn: Date };',
        'export type BillEnv_Sent = { bill: { id: string; issuedOn: Date }; errors: string[] };',
        'export type Tally_Sent = { id: string; total: bigint };',
      ].join('\n') + '\n'
    );
    writeStub(
      root,
      'portal',
      [
        'export type Bill_Str_Exp = { id: string; issuedOn: string };',
        'export type BillEnv_Exp = { id: string; issuedOn: string };',
        'export type Bill_Num_Exp = { id: string; issuedOn: number };',
        'export type Bill_Date_Exp = { id: string; issuedOn: Date };',
        'export type Tally_Exp = { id: string; total: string };',
      ].join('\n') + '\n'
    );
    stubs = [
      { service_name: 'ledger', stub_dir: path.join(root, 'ledger') },
      { service_name: 'portal', stub_dir: path.join(root, 'portal') },
    ];
    const result = await runCheck({ stubs, pairs: PAIRS });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    assert.strictEqual(result.install_ok, true);
    verdicts = byKey(result.verdicts);
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('(a) a resolver Date read as a string is compatible, with the wire note', () => {
    const v = verdicts.get('gql-wire-date-string')!;
    assert.strictEqual(v.bucket, 'compatible', JSON.stringify(v));
    assert.strictEqual(v.diagnostic, undefined);
    assert.ok((v.notes ?? []).includes(WIRE_NOTE), JSON.stringify(v.notes));
  });

  it('(b) the same Date under a single-payload envelope is compatible: unwrap first, then the wire form', () => {
    const v = verdicts.get('gql-wire-envelope')!;
    assert.strictEqual(v.bucket, 'compatible', JSON.stringify(v));
  });

  it('(c) a client reading the date as a number is still incompatible, named without probe-local names', () => {
    const v = verdicts.get('gql-wire-number')!;
    assert.strictEqual(v.bucket, 'incompatible', JSON.stringify(v));
    assert.ok(v.diagnostic!.includes('issuedOn'), v.diagnostic);
    assert.doesNotMatch(v.diagnostic!, /GqlWire|JsonWire|GqlComparand|GqlExpected|sentWire/, v.diagnostic);
  });

  it('(d) a client that declares the Date agrees as declared, and carries no wire note', () => {
    const v = verdicts.get('gql-wire-declared')!;
    assert.strictEqual(v.bucket, 'compatible', JSON.stringify(v));
    assert.ok(!(v.notes ?? []).includes(WIRE_NOTE), JSON.stringify(v.notes));
  });

  it('(e) a bigint against a string keeps the verdict it had before the wire form (#1875)', () => {
    const v = verdicts.get('gql-wire-bigint')!;
    assert.strictEqual(v.bucket, 'incompatible', JSON.stringify(v));
  });
});

describe('the wire form leaves the other probes byte for byte (carrick#4275)', () => {
  const sha = (protocol: CheckPairSpec['protocol'], type_kind: CheckPairSpec['type_kind']) =>
    createHash('sha256')
      .update(
        buildProbe(
          {
            pair_key: 'k',
            protocol,
            type_kind,
            producer: { service_name: 'api', alias: 'P' },
            consumer: { service_name: 'web', alias: 'C' },
          },
          (s) => `@carrick/${s}`
        ).source
      )
      .digest('hex');

  it('an http response and an http request probe are unchanged', () => {
    assert.strictEqual(sha('http', 'response'), '3d43a49784e9b26f445d69911f5a0447c405266f5b9346b8b5bd2ffd11805042');
    assert.strictEqual(sha('http', 'request'), '6e3fc379ad433fc974c3f5b895d0fe670856ed3dd470f01833a35468dd171d89');
  });

  it('a GraphQL request, a socket and a pub/sub probe are unchanged', () => {
    assert.strictEqual(sha('graphql', 'request'), '11019005ce88ad2fe800a45170885754f3cca863fc3f972aadde83f65e8e2447');
    assert.strictEqual(sha('socket', 'response'), '5a1661ba37d277a5a0ce8fd4210d0dd1a331dc7a853ddfb72ad94127ceb70fcc');
    assert.strictEqual(sha('pubsub', 'response'), '5a1661ba37d277a5a0ce8fd4210d0dd1a331dc7a853ddfb72ad94127ceb70fcc');
  });
});
