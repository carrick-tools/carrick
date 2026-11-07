/**
 * carrick#2474 R1 part 1: a verified `op` verdict carries the declarations its
 * member resolves to on that receiver, spelled as a position row's `decl_file`
 * and `decl_line`. The package is invented: a factory returning an
 * interface-typed app whose `get` and `post` take `(path, handler)`, a
 * request type that also declares `get(name)`, and a class with its own `get`.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SERVER = `export type Handler = (request: Request, response: unknown) => void;
export interface Request {
  get(name: string): string | undefined;
}
export interface App {
  get(path: string, handler: Handler): this;
  post(path: string, handler: Handler): this;
}
export default function make(): App;
export declare function request(): Request;
export declare class Server {
  constructor();
  get(path: string, handler: Handler): this;
}
`;

interface Verdict {
  verdict: string;
  reason?: string;
  declarations?: Array<{ decl_file: string; decl_line: number }>;
}

describe('verify_library_claims: declarations of a verified op (carrick#2474)', () => {
  let root: string;
  let client: SidecarClient;
  let requestId = 0;

  const P = 'fixture-server';
  const check = (id: string, exported: string, receiver: string, claim: Record<string, unknown>) => ({
    claim_id: id,
    package: P,
    export: exported,
    role: 'server_framework',
    receiver,
    claim,
  });
  const route = (member: string, of: string, parts: Record<string, unknown> = {}) => ({
    kind: 'op',
    op: 'receive',
    member,
    name: { arg: 0 },
    of,
    ...parts,
  });
  const maker = (member: string | null, form: 'call' | 'new' = 'call') => ({ kind: 'make', form, member });

  const send = async (checks: unknown[], extra: Record<string, unknown> = {}): Promise<Verdict[]> => {
    const response = await client.send<{ status: string; verdicts?: Verdict[]; errors?: string[] }>(
      { request_id: `decl-${requestId++}`, action: 'verify_library_claims', from_dir: root, checks, ...extra },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    return response.verdicts!;
  };

  const lineOf = (needle: string): number =>
    SERVER.split('\n').findIndex(line => line.includes(needle)) + 1;
  // Inside the service root, so named as a position row names it: root-relative.
  const file = 'node_modules/fixture-server/index.d.ts';

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-claims-decl-'));
    const write = (rel: string, text: string) => {
      const target = path.join(root, rel);
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.writeFileSync(target, text);
    };
    write(
      'tsconfig.json',
      JSON.stringify({
        compilerOptions: { target: 'es2020', module: 'commonjs', moduleResolution: 'node', strict: true, skipLibCheck: true, types: [] },
        include: ['src/**/*.ts'],
      })
    );
    write('package.json', JSON.stringify({ name: 'service' }));
    write('src/index.ts', 'export const service = 1;\n');
    write('node_modules/fixture-server/package.json', JSON.stringify({ name: P, version: '1.0.0', types: 'index.d.ts' }));
    write('node_modules/fixture-server/index.d.ts', SERVER);
    client = new SidecarClient();
    await client.start();
    const ready = await client.send<{ status: string }>({ request_id: 'init', action: 'init', repo_root: root });
    assert.strictEqual(ready.status, 'ready');
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('the app’s get and the request’s get resolve to different declarations; a class member to its own', async () => {
    const [appMake, appGet, appPost, reqMake, reqGet, classMake, classGet] = await send([
      check('a0', 'default', 'export', maker(null)),
      check('a1', 'default', 'instance:()', route('get', 'instance:()')),
      check('a2', 'default', 'instance:()', route('post', 'instance:()')),
      check('r0', 'request', 'export', maker(null)),
      check('r1', 'request', 'instance:()', { ...route('get', 'instance:()'), op: 'receive' }),
      check('c0', 'Server', 'export', maker(null, 'new')),
      check('c1', 'Server', 'instance:new', route('get', 'instance:new')),
    ]);
    // Makers carry none: only an op names a member.
    assert.strictEqual(appMake.declarations, undefined);
    assert.strictEqual(reqMake.declarations, undefined);
    assert.strictEqual(classMake.declarations, undefined);
    assert.deepStrictEqual(appGet.declarations, [{ decl_file: file, decl_line: lineOf('get(path: string, handler: Handler): this;') }]);
    assert.deepStrictEqual(appPost.declarations, [{ decl_file: file, decl_line: lineOf('post(path') }]);
    assert.deepStrictEqual(reqGet.verdict, 'verified', JSON.stringify(reqGet));
    assert.deepStrictEqual(reqGet.declarations, [{ decl_file: file, decl_line: lineOf('get(name: string)') }]);
    const classLine = SERVER.split('\n').map((l, i) => (l.includes('get(path: string, handler: Handler): this;') ? i + 1 : 0)).filter(Boolean)[1];
    assert.deepStrictEqual(classGet.declarations, [{ decl_file: file, decl_line: classLine }]);
    assert.notDeepStrictEqual(appGet.declarations, reqGet.declarations);
    assert.notDeepStrictEqual(appGet.declarations, classGet.declarations);
  });

  it('a service at the scan root names them the same with the scan root sent', async () => {
    const checks = [check('a0', 'default', 'export', maker(null)), check('a1', 'default', 'instance:()', route('get', 'instance:()'))];
    const [, without] = await send(checks);
    const [, withRoot] = await send(checks, { scan_root: root });
    assert.deepStrictEqual(withRoot.declarations, [{ decl_file: file, decl_line: lineOf('get(path: string, handler: Handler): this;') }]);
    assert.deepStrictEqual(withRoot, without);
  });

  it('a failed op carries none', async () => {
    const [, bad] = await send([
      check('f0', 'default', 'export', maker(null)),
      check('f1', 'default', 'instance:()', route('get', 'instance:()', { name: { arg: 5 } })),
    ]);
    assert.strictEqual(bad.verdict, 'failed');
    assert.strictEqual(bad.declarations, undefined);
  });
});

/**
 * carrick#3044 S1: an http_client op without amendment-7 fields is judged
 * under the #1564 rules (no `protocol`), and a verified op there carries the
 * declarations of the member it judged, by the same rule as above: the
 * property for a member typed by an interface with call signatures, the
 * method otherwise. Makers and results carry none. The package is invented.
 */
const CLIENT = `export interface Put {
  (url: string, data?: unknown): Promise<unknown>;
  (url: string, options: object): Promise<unknown>;
}
export interface Inst {
  get(url: string): Promise<unknown>;
  post(url: string, data?: unknown): Promise<unknown>;
  put: Put;
}
export interface Opts { baseURL?: string }
export interface Mk { create(options?: Opts): Inst }
export declare const http: Inst;
declare const mk: Mk;
export default mk;
`;

describe('verify_library_claims: an http client op names its member declaration (carrick#3044)', () => {
  let root: string;
  let client: SidecarClient;
  let requestId = 0;

  const P = 'fixture-client';
  const check = (id: string, exported: string, receiver: string, claim: Record<string, unknown>) => ({
    claim_id: id,
    package: P,
    export: exported,
    role: 'http_client',
    receiver,
    claim,
  });
  // No `protocol`: the claim is read under the #1564 rules.
  const request = (member: string, method: string, of: Record<string, string>, parts: Record<string, unknown> = {}) => ({
    kind: 'op',
    op: 'request',
    member,
    name: { arg: 0 },
    payload: { arg: 1 },
    method,
    ...of,
    ...parts,
  });
  const lineOf = (needle: string): number => CLIENT.split('\n').findIndex(line => line.includes(needle)) + 1;
  const file = 'node_modules/fixture-client/index.d.ts';

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-claims-decl-http-'));
    const write = (rel: string, text: string) => {
      const target = path.join(root, rel);
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.writeFileSync(target, text);
    };
    write(
      'tsconfig.json',
      JSON.stringify({
        compilerOptions: { target: 'es2020', module: 'commonjs', moduleResolution: 'node', strict: true, skipLibCheck: true, types: [] },
        include: ['src/**/*.ts'],
      })
    );
    write('package.json', JSON.stringify({ name: 'service' }));
    write('src/index.ts', 'export const service = 1;\n');
    write('node_modules/fixture-client/package.json', JSON.stringify({ name: P, version: '1.0.0', types: 'index.d.ts' }));
    write('node_modules/fixture-client/index.d.ts', CLIENT);
    client = new SidecarClient();
    await client.start();
    const ready = await client.send<{ status: string }>({ request_id: 'init', action: 'init', repo_root: root });
    assert.strictEqual(ready.status, 'ready');
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('an http client op under the 1564 rules names its member declaration', async () => {
    const response = await client.send<{
      status: string;
      verdicts?: Verdict[];
      errors?: string[];
    }>(
      {
        request_id: `decl-http-${requestId++}`,
        action: 'verify_library_claims',
        from_dir: root,
        checks: [
          check('e0', 'http', 'export', request('post', 'POST', { on: 'export' })),
          check('e1', 'http', 'export', request('get', 'GET', { on: 'export' }, { payload: undefined })),
          check('e2', 'http', 'export', request('put', 'PUT', { on: 'export' })),
          check('m0', 'default', 'export', { kind: 'make', form: 'call', member: 'create', base: { arg: 0, key: 'baseURL' } }),
          check('m1', 'default', 'instance:create', request('post', 'POST', { of: 'instance:create' })),
          check('m2', 'default', 'instance:create', request('put', 'PUT', { of: 'instance:create' })),
        ],
      },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    const [post, get, put, make, instPost, instPut] = response.verdicts!;
    for (const verdict of [post, get, put, make, instPost, instPut]) {
      assert.strictEqual(verdict.verdict, 'verified', JSON.stringify(verdict));
    }
    const post_line = lineOf('post(url: string, data?: unknown)');
    assert.deepStrictEqual(post.declarations, [{ decl_file: file, decl_line: post_line }]);
    assert.deepStrictEqual(get.declarations, [{ decl_file: file, decl_line: lineOf('get(url: string)') }]);
    // A member typed by an interface with call signatures names the property.
    assert.deepStrictEqual(put.declarations, [{ decl_file: file, decl_line: lineOf('put: Put;') }]);
    assert.deepStrictEqual(instPost.declarations, [{ decl_file: file, decl_line: post_line }]);
    assert.deepStrictEqual(instPut.declarations, [{ decl_file: file, decl_line: lineOf('put: Put;') }]);
    // A maker names none: only an op names a member.
    assert.strictEqual(make.declarations, undefined);
  });
});
