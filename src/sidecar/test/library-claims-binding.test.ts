/**
 * A procedure named by the variable that holds it (carrick#4909, contract
 * amendment 13: `name: { bound: 'binding' }`).
 *
 * - On a `receive` op with `protocol: 'rpc'` and a `handler`, the op call's
 *   value is a procedure. The member's declared return has a call signature
 *   (`result_not_callable`), the `payload` slot exists on it, and a
 *   `method_key` with `maker: true` exists on the maker's signature
 *   (`slot_missing`).
 * - Its method words are the route words, not the RPC words.
 * - A make's name slot with `ref: 'function'` holds when every part of the
 *   slot has a call signature (`ref_not_callable`).
 * - `maker` is a flag of that `method_key` slot only (`claim_invalid`).
 *
 * The packages are invented.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project } from 'ts-morph';
import { LibraryClaimsVerifier } from '../src/library-claims.js';
import { parseRequest } from '../src/validators.js';
import type { LibraryCheck } from '../src/types.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: {
    target: 'es2020',
    module: 'commonjs',
    moduleResolution: 'node',
    lib: ['es2020', 'dom'],
    strict: true,
    esModuleInterop: true,
    skipLibCheck: true,
    types: [],
  },
  include: ['src/**/*.ts'],
});

const CORE = `interface Ctx<D> { data: D }
interface HandlerStep<I> { handler<R>(fn: (c: Ctx<I>) => R): Stub<I, R> }
interface Stub<I, R> { (opts: { data: I }): Promise<Awaited<R>> }
interface Built<I> extends HandlerStep<I> { use(m: unknown[]): Built<I>; validate<J>(v: (d: J) => unknown): Built<J> }
export declare const defineFn: (opts?: { method?: "GET" | "POST" }) => Built<undefined>;
export declare const defineBad: (opts?: { method?: "GET" | "POST" }) => { handler<R>(fn: (c: Ctx<undefined>) => R): { ok: boolean } };
export declare const defineNoOpts: () => Built<undefined>;
`;

const RPC = `export { defineFn, defineBad, defineNoOpts } from "@fixture/rpc-core";
export declare function useFn<T extends (...a: any[]) => Promise<any>>(fn: T): (...a: Parameters<T>) => ReturnType<T>;
export declare function useText(fn: string): () => void;
`;

const PKG = '@fixture/rpc';
let sequence = 0;
const check = (exported: string, role: string, receiver: string, claim: Record<string, unknown>): LibraryCheck =>
  ({ claim_id: `b${sequence++}`, package: PKG, export: exported, role, receiver, claim }) as unknown as LibraryCheck;
const make = (exported: string) => check(exported, 'server_framework', 'export', { kind: 'make', form: 'call', member: null });
const binding = (exported: string, extra: Record<string, unknown> = {}) =>
  check(exported, 'server_framework', 'instance:()', {
    kind: 'op',
    op: 'receive',
    member: 'handler',
    of: 'instance:()',
    protocol: 'rpc',
    handler: { arg: 0 },
    name: { bound: 'binding' },
    method_default: 'GET',
    payload: { arg: 0, key: 'data' },
    method_key: { arg: 0, key: 'method', maker: true },
    ...extra,
  });

describe('verify_library_claims: a procedure named by its binding (amendment 13)', () => {
  let root: string;
  let verifier: LibraryClaimsVerifier;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-binding-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      // The core package is NOT declared: only the one the service imports is.
      'package.json': JSON.stringify({ name: 'svc', dependencies: { [PKG]: '1.0.0' } }),
      'src/index.ts': 'export const service = 1;\n',
      [`node_modules/${PKG}/package.json`]: JSON.stringify({ name: PKG, version: '1.0.0', types: 'index.d.ts' }),
      [`node_modules/${PKG}/index.d.ts`]: RPC,
      'node_modules/@fixture/rpc-core/package.json': JSON.stringify({ name: '@fixture/rpc-core', version: '1.0.0', types: 'index.d.ts' }),
      'node_modules/@fixture/rpc-core/index.d.ts': CORE,
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

  const verdicts = (checks: LibraryCheck[]): string[] =>
    verifier.run(root, checks, 60_000).semantics.map(r => (r.verdict === 'verified' ? 'verified' : `${r.verdict} ${r.reason}`));
  /** The last check's verdict, with the maker claim it reads beside it. */
  const judged = (exported: string, op: LibraryCheck): string => verdicts([make(exported), op])[1];

  it('verifies the labelled call, its payload slot and its maker slot', () => {
    assert.strictEqual(judged('defineFn', binding('defineFn')), 'verified');
  });

  it('verifies without a maker slot, and the default method may be unwritten', () => {
    const bare = binding('defineNoOpts', { method_key: undefined });
    assert.strictEqual(judged('defineNoOpts', bare), 'verified');
  });

  it('reads the method words as route words', () => {
    assert.strictEqual(judged('defineFn', binding('defineFn', { method_default: 'POST' })), 'verified');
    assert.strictEqual(judged('defineFn', binding('defineFn', { method_default: 'QUERY' })), 'failed method_not_word');
  });

  it('fails a member whose return is not callable', () => {
    assert.strictEqual(judged('defineBad', binding('defineBad')), 'failed result_not_callable');
  });

  it('fails a payload slot the call signature lacks', () => {
    assert.strictEqual(judged('defineFn', binding('defineFn', { payload: { arg: 0, key: 'body' } })), 'failed slot_missing');
    assert.strictEqual(judged('defineFn', binding('defineFn', { payload: { arg: 3 } })), 'failed slot_missing');
  });

  it('fails a maker slot the maker signature lacks', () => {
    assert.strictEqual(
      judged('defineFn', binding('defineFn', { method_key: { arg: 0, key: 'verb', maker: true } })),
      'failed slot_missing'
    );
    assert.strictEqual(
      judged('defineNoOpts', binding('defineNoOpts', { method_key: { arg: 0, key: 'method', maker: true } })),
      'failed slot_missing'
    );
  });

  it('is claim_invalid unless the op is an rpc receive with a handler', () => {
    assert.strictEqual(judged('defineFn', binding('defineFn', { handler: undefined })), 'unchecked claim_invalid');
    assert.strictEqual(judged('defineFn', binding('defineFn', { protocol: undefined })), 'unchecked claim_invalid');
  });

  it('is claim_invalid when `maker` sits anywhere but the method_key of this op', () => {
    assert.strictEqual(
      judged('defineFn', binding('defineFn', { payload: { arg: 0, key: 'data', maker: true } })),
      'unchecked claim_invalid'
    );
    assert.strictEqual(
      judged('defineFn', binding('defineFn', { name: { literal: 'x' } })),
      'unchecked claim_invalid'
    );
  });

  it('is claim_invalid when a binding op\'s method_key lacks `maker: true`', () => {
    assert.strictEqual(
      judged('defineFn', binding('defineFn', { method_key: { arg: 0, key: 'method' } })),
      'unchecked claim_invalid'
    );
    assert.strictEqual(
      judged('defineFn', binding('defineFn', { method_key: { arg: 0, key: 'method', maker: false } })),
      'unchecked claim_invalid'
    );
  });

  it('verifies a make whose name slot is a function (`ref`)', () => {
    const useMake = check('useFn', 'http_client', 'export', {
      kind: 'make',
      form: 'call',
      member: null,
      name: { arg: 0, ref: 'function' },
    });
    const useRequest = check('useFn', 'http_client', 'instance:()', {
      kind: 'op',
      op: 'request',
      member: null,
      of: 'instance:()',
      protocol: 'rpc',
      name: { bound: 'maker' },
      payload: { arg: 0, key: 'data' },
    });
    // The request reads the instance's call through a rest of the maker's own type parameter: that is the
    // existing op rule's answer, not this amendment's.
    assert.strictEqual(verdicts([useMake, useRequest])[0], 'verified');
  });

  it('fails a make whose `ref` slot is not callable', () => {
    const useText = check('useText', 'http_client', 'export', {
      kind: 'make',
      form: 'call',
      member: null,
      name: { arg: 0, ref: 'function' },
    });
    // The make is read under the v2 rules by the request that names its instance.
    const useRequest = check('useText', 'http_client', 'instance:()', {
      kind: 'op',
      op: 'request',
      member: null,
      of: 'instance:()',
      protocol: 'rpc',
      name: { bound: 'maker' },
    });
    assert.strictEqual(verdicts([useText, useRequest])[0], 'failed ref_not_callable');
  });

  it('still answers part_unbuilt for a make `ref` on an `of` make', () => {
    const ofMake = check('useFn', 'http_client', 'instance:()', {
      kind: 'make',
      form: 'call',
      member: 'x',
      of: 'instance:()',
      name: { arg: 0, ref: 'function' },
    });
    assert.strictEqual(verdicts([ofMake])[0], 'unchecked part_unbuilt');
  });

  it('takes `bound: binding` and the maker flag on the wire', () => {
    const request = (claim: Record<string, unknown>) =>
      parseRequest({
        request_id: 'r',
        action: 'verify_library_claims',
        from_dir: '/x',
        checks: [{ claim_id: 'c', package: PKG, export: 'defineFn', role: 'server_framework', receiver: 'instance:()', claim }],
      }).success;
    const op = (extra: Record<string, unknown>) => ({
      kind: 'op',
      op: 'receive',
      member: 'handler',
      of: 'instance:()',
      protocol: 'rpc',
      handler: { arg: 0 },
      name: { bound: 'binding' },
      ...extra,
    });
    assert.strictEqual(request(op({ method_key: { arg: 0, key: 'method', maker: true } })), true);
    assert.strictEqual(request(op({ method_key: { arg: 0, key: 'method', maker: 'yes' } })), false);
    assert.strictEqual(request(op({ name: { bound: 'nonsense' } })), false);
  });
});
