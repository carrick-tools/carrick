/**
 * carrick#3048 (H4 of the HTTP slice), label restructure amendment 7
 * (carrick#2434), part 4 and the part-2 name sources: `protocol` and the
 * method words each protocol allows, `name.bound` `path` and `export`, a
 * name slot read as a GraphQL `document` or a `ref`, and a `literal` name.
 * The server-framework and HTTP-client roles speak HTTP, RPC and GraphQL, so
 * each question is judged here for both.
 *
 * Every package below is invented and its declarations are hand-written, from
 * the general shape of a procedure kit, its recursive client proxy and a
 * GraphQL-over-HTTP request function.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project } from 'ts-morph';
import { LibraryClaimsVerifier } from '../src/library-claims.js';
import type { LibraryCheck } from '../src/types.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: {
    target: 'es2020',
    module: 'commonjs',
    moduleResolution: 'node',
    strict: true,
    esModuleInterop: true,
    skipLibCheck: true,
    types: [],
  },
  include: ['src/**/*.ts'],
});

// A procedure kit: `init.create()` makes a kit whose builder registers
// resolvers and whose `router` member collects them by key.
const RPC_SERVER = `export interface Proc<I, O> {
  readonly input: I;
  readonly output: O;
}
export interface Router {
  readonly isRouter: true;
}
export interface RouterRecord {
  [key: string]: Proc<any, any> | RouterRecord | Router;
}
export interface Builder {
  query<O>(resolver: (options: { input: unknown }) => O): Proc<unknown, O>;
  mutation<O>(resolver: (options: { input: unknown }) => O): Proc<unknown, O>;
}
export interface Kit {
  procedure: Builder;
  router(record: RouterRecord): Router;
}
export interface Init {
  create(): Kit;
}
export declare const init: Init;
`;

// The client side: a recursive proxy over the router's record type, whose
// leaves (a procedure) carry the calls, and a plain object that is no proxy.
const RPC_CLIENT = `export interface Proc<I, O> {
  readonly input: I;
  readonly output: O;
}
export interface RouterRecord {
  [key: string]: Proc<any, any> | RouterRecord;
}
export type Proxy<R> = {
  [K in keyof R]: R[K] extends Proc<infer I, infer O>
    ? { query(input: I): Promise<O>; mutate(input: I): Promise<O> }
    : Proxy<R[K]>;
};
export type Client<R extends RouterRecord = RouterRecord> = Proxy<R>;
export declare function createClient<R extends RouterRecord = RouterRecord>(): Client<R>;
export interface PlainClient {
  query(input: string): Promise<string>;
}
export declare function plain(): PlainClient;
`;

// GraphQL over HTTP: the document is a string, or the library's document type.
const GQL = `export interface DocumentNode {
  readonly kind: 'Document';
  readonly definitions: readonly unknown[];
}
export type DocumentLike = DocumentNode | string;
export declare function request(url: string, document: DocumentLike, variables?: object): Promise<unknown>;
export declare function bare(url: string, count: number, variables?: object): Promise<unknown>;
export declare function typedDoc(url: string, document: DocumentNode): Promise<unknown>;
export declare function keyed(options: { url: string; document: DocumentLike; count: number }): Promise<unknown>;
export declare function callbackGql(url: string, run: () => void): Promise<unknown>;
`;

let sequence = 0;
const check = (pkg: string, exported: string, role: string, receiver: string, claim: Record<string, unknown>): LibraryCheck =>
  ({
    claim_id: `q${sequence++}`,
    package: pkg,
    export: exported,
    role,
    receiver,
    claim,
  }) as unknown as LibraryCheck;

// Server side.
const KIT = 'instance:create';
const create = check('fixture-rpc-server', 'init', 'server_framework', 'export', { kind: 'make', form: 'call', member: 'create' });
const router = check('fixture-rpc-server', 'init', 'server_framework', KIT, {
  kind: 'scope',
  member: 'router',
  of: KIT,
  collects: { arg: 0, join: '.' },
});
const procedure = (extra: Record<string, unknown> = {}, member = 'query', role = 'server_framework') =>
  check('fixture-rpc-server', 'init', role, KIT, {
    kind: 'op',
    op: 'receive',
    member,
    path: ['procedure'],
    of: KIT,
    name: { bound: 'key' },
    handler: { arg: 0 },
    ...extra,
  });

// Client side (role http_client).
const clientMake = check('fixture-rpc-client', 'createClient', 'http_client', 'export', {
  kind: 'make',
  form: 'call',
  member: null,
});
const viaPath = (exported: string, member: string, extra: Record<string, unknown> = {}) =>
  check('fixture-rpc-client', exported, 'http_client', 'type', {
    kind: 'op',
    op: 'request',
    member,
    of: 'type',
    protocol: 'rpc',
    method: member.toUpperCase(),
    name: { bound: 'path', join: '.' },
    payload: { arg: 0 },
    ...extra,
  });

// GraphQL over HTTP (role http_client).
const gql = (exported: string, claim: Record<string, unknown>) =>
  check('fixture-gql-http', exported, 'http_client', 'export', {
    kind: 'op',
    op: 'request',
    member: null,
    on: 'export',
    protocol: 'graphql',
    base: { arg: 0 },
    ...claim,
  });

describe('verify_library_claims: protocol, method words and name sources (carrick#3048)', () => {
  let root: string;
  let verifier: LibraryClaimsVerifier;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-protocol-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'svc' }),
      'src/index.ts': 'export const service = 1;\n',
    };
    for (const [name, text] of [
      ['fixture-rpc-server', RPC_SERVER],
      ['fixture-rpc-client', RPC_CLIENT],
      ['fixture-gql-http', GQL],
    ]) {
      files[`node_modules/${name}/package.json`] = JSON.stringify({ name, version: '1.0.0', types: 'index.d.ts' });
      files[`node_modules/${name}/index.d.ts`] = text;
    }
    for (const [rel, text] of Object.entries(files)) {
      const file = path.join(root, rel);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, text);
    }
    verifier = new LibraryClaimsVerifier(new Project({ tsConfigFilePath: path.join(root, 'tsconfig.json') }));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  const verdicts = (checks: LibraryCheck[]): string[] =>
    verifier.run(root, checks, 60_000).semantics.map(r => (r.verdict === 'verified' ? 'verified' : `${r.verdict} ${r.reason}`));
  /** The last check's verdict, read beside the checks that make its receiver. */
  const last = (checks: LibraryCheck[]): string => verdicts(checks).at(-1)!;

  it('protocol: a value the role may not speak is protocol_role, an unknown one claim_invalid', () => {
    assert.strictEqual(last([create, router, procedure({ protocol: 'socket', method: 'QUERY' })]), 'unchecked protocol_role');
    assert.strictEqual(last([create, router, procedure({ protocol: 'message', method: 'QUERY' })]), 'unchecked protocol_role');
    assert.strictEqual(last([create, router, procedure({ protocol: 'grpc', method: 'QUERY' })]), 'unchecked claim_invalid');
    assert.strictEqual(last([create, router, procedure({ protocol: 'rpc', method: 'QUERY' })]), 'verified');
    assert.strictEqual(last([create, router, procedure({ protocol: 'graphql', method: 'QUERY' })]), 'verified');
  });

  it('method words: rpc takes the rpc words, and the member is the method', () => {
    assert.strictEqual(last([create, router, procedure({ protocol: 'rpc', method: 'QUERY' })]), 'verified');
    assert.strictEqual(last([create, router, procedure({ protocol: 'rpc', method: 'GET' })]), 'failed method_not_member_verb');
    assert.strictEqual(last([create, router, procedure({ protocol: 'rpc', method: 'MUTATION' }, 'mutation')]), 'verified');
    // An http route word on a graphql op, and a graphql word beside an http route.
    assert.strictEqual(last([create, router, procedure({ protocol: 'graphql', method: 'MUTATE' }, 'mutation')]), 'failed method_not_member_verb');
  });

  it('a key-bound procedure needs a method word on http only', () => {
    assert.strictEqual(last([create, router, procedure({ protocol: 'rpc' })]), 'verified');
    assert.strictEqual(last([create, router, procedure({ protocol: 'graphql' })]), 'verified');
    assert.strictEqual(last([create, router, procedure({ protocol: 'http' })]), 'unchecked claim_invalid');
    assert.strictEqual(last([create, router, procedure({})]), 'unchecked claim_invalid');
    // The word a route has, on http, as before.
    assert.strictEqual(last([create, router, procedure({ protocol: 'http', method: 'GET' })]), 'verified');
    assert.strictEqual(last([create, router, procedure({ method: 'GET' })]), 'verified');
  });

  it('bound path: a recursive proxy of procedures holds the member, a plain object does not', () => {
    assert.strictEqual(last([clientMake, viaPath('Client', 'query')]), 'verified');
    assert.strictEqual(last([clientMake, viaPath('Client', 'mutate')]), 'verified');
    assert.strictEqual(last([clientMake, viaPath('PlainClient', 'query')]), 'failed path_not_proxy');
    // A member the proxy does not declare, and a method word that is not the member's.
    assert.strictEqual(last([clientMake, viaPath('Client', 'subscribe')]), 'failed member_missing');
    assert.strictEqual(last([clientMake, viaPath('Client', 'query', { method: 'GET' })]), 'failed method_not_member_verb');
  });

  it('the rpc client make is read with its package, not as an http factory', () => {
    assert.deepStrictEqual(verdicts([clientMake, viaPath('Client', 'query')]), ['verified', 'verified']);
  });

  it('bound export: a receive op with a handler only', () => {
    const exported = (extra: Record<string, unknown>) =>
      check('fixture-rpc-server', 'init', 'server_framework', KIT, {
        kind: 'op',
        op: 'receive',
        member: 'router',
        of: KIT,
        name: { bound: 'export' },
        handler: { arg: 0 },
        ...extra,
      });
    // `router` takes a record, not a function: the handler slot fails, which is a real verdict, not a name fault.
    assert.notStrictEqual(last([create, exported({})]), 'unchecked claim_invalid');
    assert.strictEqual(last([create, exported({ handler: undefined })]), 'unchecked claim_invalid');
  });

  it('document: a slot that accepts a document type or a string', () => {
    assert.strictEqual(last([gql('request', { name: { arg: 1, document: 'graphql' }, payload: { arg: 2 } })]), 'verified');
    assert.strictEqual(last([gql('typedDoc', { name: { arg: 1, document: 'graphql' } })]), 'verified');
    assert.strictEqual(last([gql('bare', { name: { arg: 1, document: 'graphql' }, payload: { arg: 2 } })]), 'failed name_not_document');
    assert.strictEqual(last([gql('callbackGql', { name: { arg: 1, document: 'graphql' } })]), 'failed name_not_document');
    assert.strictEqual(last([gql('request', { name: { arg: 1, document: 'sdl' }, payload: { arg: 2 } })]), 'unchecked claim_invalid');
  });

  it('document: a key of an options object', () => {
    const keyed = (key: string) =>
      gql('keyed', { base: { arg: 0, key: 'url' }, name: { arg: 0, key, document: 'graphql' } });
    assert.strictEqual(last([keyed('document')]), 'verified');
    assert.strictEqual(last([keyed('count')]), 'failed name_not_document');
  });

  it('ref: a slot that takes a function or a class', () => {
    const ref = (arg: number) => gql('callbackGql', { protocol: undefined, base: undefined, name: { arg, ref: 'function' } });
    assert.strictEqual(last([ref(1)]), 'verified');
    assert.strictEqual(last([ref(0)]), 'failed ref_not_callable');
  });

  it('literal: a non-empty string', () => {
    const literal = (value: unknown) =>
      check('fixture-rpc-server', 'init', 'server_framework', KIT, {
        kind: 'op',
        op: 'receive',
        member: 'query',
        path: ['procedure'],
        of: KIT,
        name: { literal: value },
        handler: { arg: 0 },
        protocol: 'rpc',
        method: 'QUERY',
      });
    assert.strictEqual(last([create, literal('ping')]), 'verified');
    assert.strictEqual(last([create, literal('')]), 'unchecked claim_invalid');
    assert.strictEqual(last([create, literal(3)]), 'unchecked claim_invalid');
  });

  it('reads an http_client export with an amendment-7 field under the v2 rules, per package', () => {
    // The package has no other v2 field: the make carries none either.
    assert.deepStrictEqual(verdicts([clientMake, viaPath('Client', 'query')]), ['verified', 'verified']);
    // Without a field of the new grammar, the same make is read as an http factory (claim_invalid today).
    assert.deepStrictEqual(verdicts([clientMake]), ['unchecked claim_invalid']);
  });
});
