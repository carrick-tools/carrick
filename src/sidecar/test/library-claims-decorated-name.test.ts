/**
 * Contract amendment 8 (carrick#2434), seam step SB0-S: a GraphQL root field
 * written as a method decorator. The op's name is `{ bound: 'decorated' }`,
 * the decorated method's own name, or `{ first: [...] }`, the first entry the
 * call fills, read in order. The verifier holds strict D2 on EVERY overload
 * that returns a method decorator, so a no-argument overload never verifies
 * a name that would overwrite an explicit one.
 *
 * Every package below is invented and its declarations are hand-written: one
 * package per overload shape, and one that holds them all.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project } from 'ts-morph';
import { LibraryClaimsVerifier } from '../src/library-claims.js';
import { VerifyLibraryClaimsRequestSchema } from '../src/validators.js';
import type { LibraryCheck } from '../src/types.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: {
    target: 'es2020',
    module: 'commonjs',
    moduleResolution: 'node',
    lib: ['es2020'],
    strict: true,
    esModuleInterop: true,
    skipLibCheck: true,
    types: [],
  },
  include: ['src/**/*.ts'],
});

const OPTIONS = `export type ReturnTypeFunc = (returns?: void) => unknown;
export interface BaseTypeOptions {
  nullable?: boolean | 'items' | 'itemsAndList';
  defaultValue?: any;
}
export interface FieldOptions extends BaseTypeOptions {
  name?: string;
  description?: string;
  deprecationReason?: string;
  complexity?: number;
}
`;

/** Every overload a root-field decorator takes: none, a name, a type and options. */
const FULL = `${OPTIONS}
export declare function Query(): MethodDecorator;
export declare function Query(name: string): MethodDecorator;
export declare function Query(name: string, options: FieldOptions): MethodDecorator;
export declare function Query(typeFunc: ReturnTypeFunc, options?: FieldOptions): MethodDecorator;
export declare function Mutation(): MethodDecorator;
export declare function Mutation(name: string): MethodDecorator;
export declare function Mutation(typeFunc: ReturnTypeFunc, options?: FieldOptions): MethodDecorator;
`;
/** `@Query('x')`: the name is argument 0. */
const BY_STRING = `export declare function Query(name: string): MethodDecorator;
`;
/** `@Query({ name: 'y' })`: the name is the options object's `name`. */
const BY_OPTIONS = `export declare function Query(options: { name?: string; description?: string }): MethodDecorator;
`;
/** `@Query()`: the call names nothing, the method does. */
const BARE = `export type ReturnTypeFunc = (returns?: void) => unknown;
export declare function Query(): MethodDecorator;
export declare function Query(typeFunc: ReturnTypeFunc): MethodDecorator;
`;
/** A parameter decorator: never a root field. */
const PARAMS = `export declare function Query(key?: string): ParameterDecorator;
`;

const FIRST = [{ arg: 0 }, { arg: 1, key: 'name' }, { bound: 'decorated' }];
const NOT_NAME = { description: 'not_name', deprecationReason: 'not_name', nullable: 'not_name', defaultValue: 'not_name' };

let sequence = 0;
const rooted = (pkg: string, exported: string, extra: Record<string, unknown> = {}): LibraryCheck =>
  ({
    claim_id: `d${sequence++}`,
    package: pkg,
    export: exported,
    role: 'server_framework',
    receiver: 'export',
    claim: {
      kind: 'op',
      op: 'receive',
      member: null,
      on: 'export',
      form: 'decorate',
      protocol: 'graphql',
      method: exported.toUpperCase(),
      ...extra,
    },
  }) as unknown as LibraryCheck;
const full = (extra: Record<string, unknown> = {}) =>
  rooted('fixture-graph-full', 'Query', { name: { first: FIRST }, key_labels: NOT_NAME, ...extra });

describe('verify_library_claims: decorated and first names (amendment 8)', () => {
  let root: string;
  let verifier: LibraryClaimsVerifier;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-decorated-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'svc' }),
      'src/index.ts': 'export const service = 1;\n',
    };
    for (const [name, text] of [
      ['fixture-graph-full', FULL],
      ['fixture-graph-string', BY_STRING],
      ['fixture-graph-options', BY_OPTIONS],
      ['fixture-graph-bare', BARE],
      ['fixture-graph-params', PARAMS],
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

  const verdict = (one: LibraryCheck): string => {
    const [result] = verifier.run(root, [one], 60_000).semantics;
    return result.verdict === 'verified' ? 'verified' : `${result.verdict} ${result.reason}`;
  };

  describe('each overload shape verifies its own name form', () => {
    it("`@Query('x')`: argument 0, then the method", () => {
      const named = { name: { first: [{ arg: 0 }, { bound: 'decorated' }] } };
      assert.strictEqual(verdict(rooted('fixture-graph-string', 'Query', named)), 'verified');
    });

    it("`@Query({ name: 'y' })`: the options key, then the method", () => {
      const named = { name: { first: [{ arg: 0, key: 'name' }, { bound: 'decorated' }] }, key_labels: { description: 'not_name' } };
      assert.strictEqual(verdict(rooted('fixture-graph-options', 'Query', named)), 'verified');
    });

    it('`@Query()`: the method alone, where no overload takes a string', () => {
      assert.strictEqual(verdict(rooted('fixture-graph-bare', 'Query', { name: { bound: 'decorated' } })), 'verified');
    });

    it('every overload together: argument 0, then the options key, then the method', () => {
      assert.strictEqual(verdict(full()), 'verified');
      assert.strictEqual(
        verdict(rooted('fixture-graph-full', 'Mutation', { name: { first: FIRST }, key_labels: NOT_NAME })),
        'verified'
      );
    });
  });

  describe('a fallback never verifies over an explicit name', () => {
    it('refuses the method alone where some overload takes a name string', () => {
      assert.strictEqual(verdict(rooted('fixture-graph-full', 'Query', { name: { bound: 'decorated' } })), 'failed name_ambiguous');
      assert.strictEqual(verdict(rooted('fixture-graph-string', 'Query', { name: { bound: 'decorated' } })), 'failed name_ambiguous');
      assert.strictEqual(verdict(rooted('fixture-graph-options', 'Query', { name: { bound: 'decorated' } })), 'failed name_ambiguous');
    });

    it('refuses a list that leaves out argument 0', () => {
      assert.strictEqual(verdict(full({ name: { first: [{ arg: 1, key: 'name' }, { bound: 'decorated' }] } })), 'failed name_ambiguous');
    });

    it('refuses a string options key left unlabelled', () => {
      const { description: _dropped, ...rest } = NOT_NAME;
      assert.strictEqual(verdict(full({ key_labels: rest })), 'failed name_ambiguous');
    });

    it('refuses a list that leaves out the options key', () => {
      assert.strictEqual(verdict(full({ name: { first: [{ arg: 0 }, { bound: 'decorated' }] } })), 'failed name_ambiguous');
    });

    it('fails an entry that takes a string on no overload, with the first overload\'s reason', () => {
      // `Query()` has no argument 0; `Query(typeFunc)` takes a function there.
      assert.strictEqual(
        verdict(rooted('fixture-graph-bare', 'Query', { name: { first: [{ arg: 0 }, { bound: 'decorated' }] } })),
        'failed param_missing'
      );
    });
  });

  describe('the claim shape', () => {
    it('wants the method last, once, in a list of two to four', () => {
      const first = (entries: unknown[]) => verdict(full({ name: { first: entries } }));
      assert.strictEqual(first([{ bound: 'decorated' }, { arg: 0 }]), 'unchecked claim_invalid');
      assert.strictEqual(first([{ arg: 0 }]), 'unchecked claim_invalid');
      assert.strictEqual(first([{ arg: 0 }, { arg: 0 }, { bound: 'decorated' }]), 'unchecked claim_invalid');
      assert.strictEqual(
        first([{ arg: 0 }, { arg: 1, key: 'name' }, { arg: 1, key: 'description' }, { arg: 2 }, { bound: 'decorated' }]),
        'unchecked claim_invalid'
      );
    });

    it('reads a decorated or first name on a decorate receive with no handler only', () => {
      assert.strictEqual(verdict(full({ form: 'call' })), 'unchecked claim_invalid');
      assert.strictEqual(verdict(full({ handler: { arg: 2 } })), 'unchecked claim_invalid');
      assert.strictEqual(verdict(full({ name: { bound: 'decorated' }, handler: { arg: 2 } })), 'unchecked claim_invalid');
    });

    it('wants the root word on a GraphQL decorator op', () => {
      assert.strictEqual(verdict(full({ method: undefined })), 'unchecked claim_invalid');
      assert.strictEqual(verdict(full({ method: 'MUTATION' })), 'failed method_not_member_verb');
    });

    it('refuses a parameter decorator', () => {
      assert.strictEqual(verdict(rooted('fixture-graph-params', 'Query', { name: { bound: 'decorated' } })), 'failed decorate_not_decorator');
    });
  });

  describe('the request schema', () => {
    it('parses a request holding one decorated-name check and one socket check whole', () => {
      const socket = {
        claim_id: 's1',
        package: 'fixture-sockets',
        export: 'io',
        role: 'socket',
        receiver: 'export',
        claim: { kind: 'op', op: 'send', member: 'emit', on: 'export', name: { arg: 0 }, payload: { arg: 1 } },
      };
      const parsed = VerifyLibraryClaimsRequestSchema.safeParse({
        request_id: 'r1',
        action: 'verify_library_claims',
        from_dir: '/x',
        checks: [full(), rooted('fixture-graph-bare', 'Query', { name: { bound: 'decorated' } }), socket],
      });
      assert.ok(parsed.success, parsed.success ? '' : parsed.error.message);
      assert.strictEqual(parsed.data.checks.length, 3);
    });

    it('refuses a first entry that is more than a plain slot or the method', () => {
      const refused = (entry: unknown) =>
        VerifyLibraryClaimsRequestSchema.safeParse({
          request_id: 'r1',
          action: 'verify_library_claims',
          from_dir: '/x',
          checks: [full({ name: { first: [entry, { bound: 'decorated' }] } })],
        }).success;
      assert.strictEqual(refused({ arg: 0, at: ['id'] }), false);
      assert.strictEqual(refused({ arg: 0, document: 'graphql' }), false);
      assert.strictEqual(refused({ bound: 'maker' }), false);
      assert.strictEqual(refused({ arg: 0 }), true);
    });
  });
});
