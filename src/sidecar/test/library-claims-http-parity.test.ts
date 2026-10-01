/**
 * carrick#1616: HTTP parity. An HTTP claim answered in the shared
 * shape must get exactly the verdict and reason its #1564 kind gets: any HTTP
 * fixture verdict that changes under the shared checks is a regression.
 *
 * `httpCheck` is the conversion both repos apply (the scanner converts the
 * client-semantics answer after the normaliser), so it is pinned here kind by
 * kind. Then every check below goes through both actions, `verify_client_semantics`
 * in the #1564 shape and `verify_library_claims` in the shared shape, and the
 * two answers must be equal, result for result and module for module.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
import { httpCheck } from '../src/library-claims.js';
import type { SemanticsCheck } from '../src/types.js';

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

const CONFIG_CLIENT = `export type Method = 'get' | 'GET' | 'post' | 'POST';
export interface RequestConfig<D = any> {
  url?: string;
  method?: Method;
  baseURL?: string;
  data?: D;
}
export interface Instance {
  <T = any>(config: RequestConfig): Promise<T>;
  request<T = any, D = any>(config: RequestConfig<D>): Promise<T>;
  get<T = any>(url: string, config?: RequestConfig): Promise<T>;
  post<T = any, D = any>(url: string, data?: D, config?: RequestConfig<D>): Promise<T>;
}
export interface Static extends Instance {
  create(config?: RequestConfig): Instance;
}
declare const client: Static;
export default client;
`;

const PREFIX_CLIENT = `export interface Options {
  prefixUrl?: string | { href: string };
  method?: string;
  json?: unknown;
}
export interface Client {
  (url: string, options?: Options): Promise<unknown>;
  get(url: string, options?: Options): Promise<unknown>;
  post(url: string, options?: Options): Promise<unknown>;
  put(...args: [url: string, options?: Options]): Promise<unknown>;
  create(defaults?: Options | ((parent: Options) => Options)): Client;
}
declare const client: Client;
export default client;
`;

const WRONG_CLIENT = `export interface Instance {
  get(url: number): Promise<unknown>;
  post(url: string): Promise<unknown>;
  put(url: string, body: { a: string }): Promise<unknown>;
  patch(url: string, options: Record<string, string>): Promise<unknown>;
  delete(url: string, options: { headers?: Record<string, string> }): Promise<unknown>;
  head: string;
  send(): Promise<unknown>;
  call(config: { url?: string }): Promise<unknown>;
  fire(config: { url: number; method: string }): Promise<unknown>;
  go(path: string, options: { verb?: string }): Promise<unknown>;
  split(config: { url: string } | { method: string }): Promise<unknown>;
  vague(config: any): Promise<unknown>;
}
export interface Static extends Instance {
  make: string;
  build(): Instance;
  create(options: { timeout?: number }): Instance;
  spawn(options: { baseURL: number }): Instance;
  loose(options: { baseURL: any }): any;
}
declare const client: Static;
export default client;
`;

type OldClaim = SemanticsCheck['claim'];

let sequence = 0;
function old(pkg: string, receiver: string, claim: OldClaim): SemanticsCheck {
  return { claim_id: `h${sequence++}`, package: pkg, export: 'default', receiver, claim };
}

const C = '@fixture/config-http';
const P = 'fixture-prefix-http';
const W = 'fixture-wrong-http';

const FACTORY_C: OldClaim = { kind: 'factory', member: 'create', base_url_key: 'baseURL' };
const FACTORY_P: OldClaim = { kind: 'factory', member: 'create', base_url_key: 'prefixUrl' };

/** Every #1564 kind and arg form, on both receivers, right and wrong. */
const CHECKS: SemanticsCheck[] = [
  old(C, 'export', FACTORY_C),
  ...['export', 'instance:create'].flatMap(receiver => [
    old(C, receiver, { kind: 'verb', member: 'get', method: 'GET' }),
    old(C, receiver, { kind: 'verb', member: 'post', method: 'POST' }),
    old(C, receiver, { kind: 'verb_body', member: 'post', args: 'path_body' }),
    old(C, receiver, { kind: 'verb_body', member: 'get', args: 'path_options' }),
    old(C, receiver, { kind: 'verb_body', member: 'get', args: 'path_options', body_key: 'data' }),
    old(C, receiver, { kind: 'request', member: 'request', args: 'config', url_key: 'url', method_key: 'method' }),
    old(C, receiver, { kind: 'request', member: null, args: 'config', url_key: 'url', method_key: 'method' }),
    old(C, receiver, { kind: 'request', member: null, args: 'config', method_key: 'method' }),
    old(C, receiver, {
      kind: 'request_body',
      member: 'request',
      args: 'config',
      url_key: 'url',
      method_key: 'method',
      body_key: 'data',
    }),
  ]),
  old(P, 'export', FACTORY_P),
  ...['export', 'instance:create'].flatMap(receiver => [
    old(P, receiver, { kind: 'verb', member: 'put', method: 'PUT' }),
    old(P, receiver, { kind: 'verb_body', member: 'post', args: 'path_options', body_key: 'json' }),
    old(P, receiver, { kind: 'request', member: null, args: 'path_options', method_key: 'method' }),
    old(P, receiver, { kind: 'request', member: null, args: 'path_options', url_key: 'ignored', method_key: 'method' }),
    old(P, receiver, { kind: 'request_body', member: null, args: 'path_options', method_key: 'method', body_key: 'json' }),
  ]),
  old(W, 'export', { kind: 'factory', member: 'absent', base_url_key: 'baseURL' }),
  old(W, 'export', { kind: 'factory', member: 'make', base_url_key: 'baseURL' }),
  old(W, 'export', { kind: 'factory', member: 'build', base_url_key: 'baseURL' }),
  old(W, 'export', { kind: 'factory', member: 'create', base_url_key: 'baseURL' }),
  old(W, 'export', { kind: 'factory', member: 'spawn', base_url_key: 'baseURL' }),
  old(W, 'export', { kind: 'factory', member: 'loose', base_url_key: 'baseURL' }),
  old(W, 'export', { kind: 'verb', member: 'get', method: 'POST' }),
  old(W, 'export', { kind: 'verb', member: 'get', method: 'GET' }),
  old(W, 'export', { kind: 'verb', member: 'head', method: 'HEAD' }),
  old(W, 'export', { kind: 'verb_body', member: 'post', args: 'path_body' }),
  old(W, 'export', { kind: 'verb_body', member: 'put', args: 'path_body' }),
  old(W, 'export', { kind: 'verb_body', member: 'patch', args: 'path_options' }),
  old(W, 'export', { kind: 'verb_body', member: 'delete', args: 'path_options', body_key: 'body' }),
  old(W, 'export', { kind: 'request', member: 'send', args: 'config', url_key: 'url', method_key: 'method' }),
  old(W, 'export', { kind: 'request', member: 'call', args: 'config', url_key: 'url', method_key: 'method' }),
  old(W, 'export', { kind: 'request', member: 'fire', args: 'config', url_key: 'url', method_key: 'method' }),
  old(W, 'export', { kind: 'request', member: 'split', args: 'config', url_key: 'url', method_key: 'method' }),
  old(W, 'export', { kind: 'request', member: 'vague', args: 'config', url_key: 'url', method_key: 'method' }),
  old(W, 'export', { kind: 'request', member: 'go', args: 'path_options', method_key: 'method' }),
  old(W, 'export', { kind: 'request', member: null, args: 'config', url_key: 'url', method_key: 'method' }),
  old(W, 'instance:loose', { kind: 'verb', member: 'get', method: 'GET' }),
  old(W, 'instance:()', { kind: 'verb', member: 'get', method: 'GET' }),
  old(W, 'nonsense', { kind: 'verb', member: 'get', method: 'GET' }),
  old('fixture-not-installed', 'export', { kind: 'verb', member: 'get', method: 'GET' }),
];

describe('HTTP claims in the shared shape (carrick#1616 parity)', () => {
  it('converts each #1564 kind one to one', () => {
    const base = { claim_id: 'x', package: 'p', export: 'default', receiver: 'export' };
    const convert = (claim: OldClaim) => httpCheck({ ...base, claim }).claim;
    assert.strictEqual(httpCheck({ ...base, claim: FACTORY_C }).role, 'http_client');
    assert.deepStrictEqual(convert(FACTORY_C), {
      kind: 'make',
      form: 'call',
      member: 'create',
      base: { arg: 0, key: 'baseURL' },
    });
    assert.deepStrictEqual(convert({ kind: 'verb', member: 'get', method: 'GET' }), {
      kind: 'op',
      op: 'request',
      member: 'get',
      method: 'GET',
      name: { arg: 0 },
    });
    assert.deepStrictEqual(convert({ kind: 'verb_body', member: 'post', args: 'path_body' }), {
      kind: 'op',
      op: 'request',
      member: 'post',
      name: { arg: 0 },
      payload: { arg: 1 },
    });
    assert.deepStrictEqual(convert({ kind: 'verb_body', member: 'post', args: 'path_options', body_key: 'json' }), {
      kind: 'op',
      op: 'request',
      member: 'post',
      name: { arg: 0 },
      options: { arg: 1 },
      payload: { arg: 1, key: 'json' },
    });
    assert.deepStrictEqual(convert({ kind: 'verb_body', member: 'get', args: 'path_options' }), {
      kind: 'op',
      op: 'request',
      member: 'get',
      name: { arg: 0 },
      options: { arg: 1 },
    });
    assert.deepStrictEqual(
      convert({ kind: 'request', member: null, args: 'config', url_key: 'url', method_key: 'method' }),
      { kind: 'op', op: 'request', member: null, name: { arg: 0, key: 'url' }, method_key: { arg: 0, key: 'method' } }
    );
    assert.deepStrictEqual(convert({ kind: 'request', member: 'request', args: 'config', method_key: 'method' }), {
      kind: 'op',
      op: 'request',
      member: 'request',
      method_key: { arg: 0, key: 'method' },
    });
    assert.deepStrictEqual(
      convert({ kind: 'request_body', member: null, args: 'path_options', method_key: 'method', body_key: 'json' }),
      {
        kind: 'op',
        op: 'request',
        member: null,
        name: { arg: 0 },
        method_key: { arg: 1, key: 'method' },
        payload: { arg: 1, key: 'json' },
      }
    );
  });

  describe('through the sidecar', () => {
    let root: string;
    let client: SidecarClient;

    before(async () => {
      root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-claims-parity-'));
      const files: Record<string, string> = {
        'tsconfig.json': TSCONFIG,
        'src/index.ts': 'export const service = 1;\n',
        [`node_modules/${C}/package.json`]: JSON.stringify({ name: C, version: '1.4.2', types: 'index.d.ts' }),
        [`node_modules/${C}/index.d.ts`]: CONFIG_CLIENT,
        [`node_modules/${P}/package.json`]: JSON.stringify({ name: P, version: '2.0.0', types: 'index.d.ts' }),
        [`node_modules/${P}/index.d.ts`]: PREFIX_CLIENT,
        [`node_modules/${W}/package.json`]: JSON.stringify({ name: W, version: '0.3.0', types: 'index.d.ts' }),
        [`node_modules/${W}/index.d.ts`]: WRONG_CLIENT,
      };
      for (const [rel, text] of Object.entries(files)) {
        fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
        fs.writeFileSync(path.join(root, rel), text);
      }
      client = new SidecarClient();
      await client.start();
      await client.send({ request_id: 'init', action: 'init', repo_root: root });
    });

    after(async () => {
      await client.stop();
      fs.rmSync(root, { recursive: true, force: true });
    });

    it('answers every check the same way in both shapes', async () => {
      const before = await client.send<{ status: string; semantics: unknown[]; semantics_modules: unknown[] }>({
        request_id: 'old',
        action: 'verify_client_semantics',
        from_dir: root,
        checks: CHECKS,
      });
      const after = await client.send<{ status: string; verdicts: unknown[]; modules: unknown[]; duration_ms: number }>({
        request_id: 'new',
        action: 'verify_library_claims',
        from_dir: root,
        checks: CHECKS.map(httpCheck),
      });
      assert.strictEqual(before.status, 'success');
      assert.strictEqual(after.status, 'success');
      assert.deepStrictEqual(after.verdicts, before.semantics);
      assert.deepStrictEqual(after.modules, before.semantics_modules);
      assert.ok(Number.isInteger(after.duration_ms) && after.duration_ms >= 0, `duration_ms ${after.duration_ms}`);
      // The set reaches every verdict, so agreement is not agreement on one answer.
      const reasons = new Set(
        (before.semantics as Array<{ verdict: string; reason?: string }>).map(r => `${r.verdict} ${r.reason ?? ''}`.trim())
      );
      for (const expected of [
        'verified',
        'failed member_missing',
        'failed member_not_callable',
        'failed param_missing',
        'failed key_missing',
        'failed key_not_string',
        'failed method_not_member_verb',
        'failed path_not_string',
        'failed body_not_open',
        'failed options_not_object',
        'unchecked member_untyped',
        'unchecked factory_unresolved',
        'unchecked receiver_invalid',
        'unchecked module_unresolved',
      ]) {
        assert.ok(reasons.has(expected), `no check reached ${expected}: ${[...reasons].join(', ')}`);
      }
    });
  });
});
