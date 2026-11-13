/**
 * S2a (carrick#2434 amendment 11, cloud#2743 item 5): a `make` element may
 * carry `protocol: "graphql"`. It is allowed on `server_framework` only, and
 * `runs` is required. The package below is invented: a function that takes a
 * schema and an endpoint path and returns a request handler.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project } from 'ts-morph';
import { LibraryClaimsVerifier } from '../src/library-claims.js';
import { settledServerChecks } from '../src/validators.js';
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

const GQL_SERVER = `export interface Schema {
  readonly kind: 'Schema';
}
export type SchemaSource = () => Schema;
export interface GqlRequest {
  body: string;
}
export type RequestHandler = (request: GqlRequest) => Promise<string>;
export declare function createGqlServer(options: { schema: SchemaSource; endpoint?: string }): RequestHandler;
export declare function createEagerServer(options: { schema: Schema; endpoint?: string }): RequestHandler;
export interface GraphQLSchemaLike {
  readonly kind: 'Schema';
  readonly types: readonly string[];
}
export declare function createObjectServer(options: { schema: GraphQLSchemaLike; endpoint?: string }): RequestHandler;
export declare function createSdlServer(options: { schema: string }): RequestHandler;
export declare function createLooseServer(options: { schema: any }): RequestHandler;
export declare function createUnknownServer(options: { schema: unknown }): RequestHandler;
export declare function noSchema(options: { endpoint: string }): RequestHandler;
`;

let sequence = 0;
const make = (role: string, claim: Record<string, unknown>, exported = 'createGqlServer', pkg = 'fixture-gql-server'): LibraryCheck =>
  ({
    claim_id: `g${sequence++}`,
    package: pkg,
    export: exported,
    role,
    receiver: 'export',
    claim: { kind: 'make', form: 'call', member: null, protocol: 'graphql', ...claim },
  }) as unknown as LibraryCheck;

describe('verify_library_claims: a graphql make (S2a)', () => {
  let root: string;
  let verifier: LibraryClaimsVerifier;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-gql-make-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'svc' }),
      'src/index.ts': 'export const service = 1;\n',
      'node_modules/fixture-gql-server/package.json': JSON.stringify({ name: 'fixture-gql-server', version: '1.0.0', types: 'index.d.ts' }),
      'node_modules/fixture-gql-server/index.d.ts': GQL_SERVER,
    };
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

  const verdict = (check: LibraryCheck): string => {
    const r = verifier.run(root, [check], 60_000).semantics[0];
    return r.verdict === 'verified' ? 'verified' : `${r.verdict} ${r.reason}`;
  };

  it('verified on a server_framework make with runs, and with a prefix', () => {
    assert.strictEqual(verdict(make('server_framework', { runs: { arg: 0, key: 'schema' } })), 'verified');
    assert.strictEqual(
      verdict(make('server_framework', { runs: { arg: 0, key: 'schema' }, prefix: { arg: 0, key: 'endpoint' } })),
      'verified'
    );
  });

  it('runs is verified on a schema object or a factory function, by declared type only', () => {
    const runs = { runs: { arg: 0, key: 'schema' } };
    assert.strictEqual(verdict(make('server_framework', runs, 'createObjectServer')), 'verified');
    assert.strictEqual(verdict(make('server_framework', runs, 'createEagerServer')), 'verified');
    assert.strictEqual(verdict(make('server_framework', runs, 'createGqlServer')), 'verified');
  });

  it('runs on a primitive fails runs_not_schema; any and unknown read unchecked runs_untyped', () => {
    const runs = { runs: { arg: 0, key: 'schema' } };
    assert.strictEqual(verdict(make('server_framework', runs, 'createSdlServer')), 'failed runs_not_schema');
    assert.strictEqual(verdict(make('server_framework', runs, 'createLooseServer')), 'unchecked runs_untyped');
    assert.strictEqual(verdict(make('server_framework', runs, 'createUnknownServer')), 'unchecked runs_untyped');
  });

  it('a graphql make whose runs key is absent fails key as before', () => {
    assert.strictEqual(
      verdict(make('server_framework', { runs: { arg: 0, key: 'nope' } }, 'createObjectServer')),
      'failed key_missing'
    );
  });

  it('refused without runs', () => {
    assert.strictEqual(verdict(make('server_framework', {})), 'unchecked claim_invalid');
    assert.strictEqual(verdict(make('server_framework', { prefix: { arg: 0, key: 'endpoint' } })), 'unchecked claim_invalid');
  });

  it('refused on http_client and on every other role as protocol_role', () => {
    const claim = { runs: { arg: 0, key: 'schema' } };
    assert.strictEqual(verdict(make('http_client', claim)), 'unchecked protocol_role');
    assert.strictEqual(verdict(make('graphql_client', claim)), 'unchecked protocol_role');
    assert.strictEqual(verdict(make('broker', claim)), 'unchecked protocol_role');
  });

  it('refuses a name, base, handler or key_labels beside it, and any protocol but graphql', () => {
    const runs = { arg: 0, key: 'schema' };
    assert.strictEqual(verdict(make('server_framework', { runs, name: { arg: 0 } })), 'unchecked claim_invalid');
    assert.strictEqual(verdict(make('server_framework', { runs, base: { arg: 0 } })), 'unchecked claim_invalid');
    assert.strictEqual(verdict(make('server_framework', { runs, handler: { arg: 0 } })), 'unchecked claim_invalid');
    assert.strictEqual(verdict(make('server_framework', { runs, key_labels: {} })), 'unchecked claim_invalid');
    assert.strictEqual(verdict(make('server_framework', { runs, protocol: 'rpc' })), 'unchecked claim_invalid');
  });

  it('the request validator keeps a make protocol (the make schema is strict)', () => {
    const check = make('server_framework', { runs: { arg: 0, key: 'schema' } });
    assert.deepStrictEqual(settledServerChecks([check]).refused, 0);
    assert.strictEqual((settledServerChecks([check]).checks[0].claim as { protocol?: string }).protocol, 'graphql');
  });
});
