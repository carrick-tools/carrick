/**
 * carrick#3047 (H3 of the HTTP slice), label restructure amendment 7
 * (carrick#2434), part 3: the verifier judges `form: set` (a send), `new`
 * (a send or a request), `decorate` (a receive, and a scope) and the
 * `returns` field of a receive op. `override`, and `set` on a receive, stay
 * `part_unbuilt`. Absorbs carrick#3022's verifier half (`set`).
 *
 * Every package below is invented and its declarations are hand-written.
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
    lib: ['es2020', 'dom'],
    strict: true,
    esModuleInterop: true,
    skipLibCheck: true,
    types: [],
  },
  include: ['src/**/*.ts'],
});

const CTX = `export interface BaseCtx {
  status: number;
  length: number;
  readonly ip: string;
  json(body: unknown): void;
  get type(): string;
}
export interface Ctx<S = {}, B = unknown> extends BaseCtx {
  state: S;
  body: B;
  response: { body: unknown };
}
export declare function router(): void;
`;
// The body is declared in another package: the router hands the context on.
const CTX_ROUTER = `import type { Ctx } from 'fixture-ctx';
export interface RouterContext extends Ctx {
  params: Record<string, string>;
}
`;
const REPLY = `export interface BodyLike { size: number }
export interface Init { status?: number }
export declare class Reply {
  constructor(body?: BodyLike | null, init?: Init);
  static json(data: unknown, init?: Init): Reply;
}
export declare class AppReply extends Reply {
  static json<J>(body: J, init?: Init): AppReply;
}
export declare function makeReply(body: unknown): Reply;
export declare class Fetcher {
  constructor(url: string, init?: object);
}
`;
const ROUTES = `export interface Req { path: string }
export interface Routes {
  get<R>(path: string, handler: (req: Req) => R | Promise<R>): void;
  use(path: string, fn: (ctx: any) => any): void;
  all(path: string, fn: (req: Req) => void): void;
  post<T>(path: string, handler: (req: Req) => { data: T } | Promise<{ data: T }>): void;
}
export declare const routes: Routes;
`;
const DECORATORS = `export declare function Get(path?: string): MethodDecorator;
export declare function Controller(prefix?: string): ClassDecorator;
export declare function Body(): ParameterDecorator;
`;

let sequence = 0;
const check = (
  pkg: string,
  exported: string,
  receiver: string,
  claim: Record<string, unknown>,
  role = 'server_framework'
): LibraryCheck =>
  ({ claim_id: `f${sequence++}`, package: pkg, export: exported, role, receiver, claim }) as unknown as LibraryCheck;

const setSend = (member: string | null, extra: Record<string, unknown> = {}, exported = 'Ctx', pkg = 'fixture-ctx') =>
  check(pkg, exported, 'type', { kind: 'op', op: 'send', member, of: 'type', form: 'set', payload: { arg: 0 }, ...extra });
const newSend = (pkg: string, exported: string, extra: Record<string, unknown> = {}) =>
  check(pkg, exported, 'export', { kind: 'op', op: 'send', member: null, on: 'export', form: 'new', payload: { arg: 0 }, ...extra });
const returning = (member: string, returns: Record<string, unknown>, extra: Record<string, unknown> = {}) =>
  check('fixture-routes', 'routes', 'export', {
    kind: 'op',
    op: 'receive',
    member,
    on: 'export',
    name: { arg: 0 },
    handler: { arg: 1 },
    returns,
    ...extra,
  });
const decorating = (exported: string, extra: Record<string, unknown> = {}) =>
  check('fixture-decorators', exported, 'export', {
    kind: 'op',
    op: 'receive',
    member: null,
    on: 'export',
    form: 'decorate',
    name: { arg: 0 },
    ...extra,
  });

describe('verify_library_claims: forms and returns (carrick#3047)', () => {
  let root: string;
  let verifier: LibraryClaimsVerifier;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-forms-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'svc' }),
      'src/index.ts': 'export const service = 1;\n',
    };
    for (const [name, text] of [
      ['fixture-ctx', CTX],
      ['fixture-ctx-router', CTX_ROUTER],
      ['fixture-reply', REPLY],
      ['fixture-routes', ROUTES],
      ['fixture-decorators', DECORATORS],
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
  const verdict = (one: LibraryCheck): string => verdicts([one])[0];
  const labels = (checks: LibraryCheck[], typePackages: Record<string, string[]> = {}) =>
    verifier.labelledMembers(root, checks, 60_000, undefined, typePackages);
  const read = (file: string, key: string): string =>
    fs.readFileSync(path.join(root, 'node_modules', file), 'utf8').slice(Number(key.split(':').pop()));

  describe('set', () => {
    it('labels an open writable property in sets, and leaves sends as they were', () => {
      const found = labels([setSend('body')]);
      assert.deepStrictEqual(found.outcomes, { verified: 1 });
      assert.strictEqual(found.sends.size, 0);
      const [key] = [...found.sets.keys()];
      assert.strictEqual(found.sets.size, 1);
      assert.match(key, /fixture-ctx\/index\.d\.ts:\d+$/);
      assert.ok(read('fixture-ctx/index.d.ts', key).startsWith('body: B;'), key);
      assert.deepStrictEqual(found.sets.get(key), { arg: 0 });
    });

    it('refuses a closed type, a readonly or get-only property and a method', () => {
      assert.deepStrictEqual(labels([setSend('status')]).outcomes, { 'failed set_not_open': 1 });
      assert.deepStrictEqual(labels([setSend('ip')]).outcomes, { 'failed set_readonly': 1 });
      assert.deepStrictEqual(labels([setSend('type')]).outcomes, { 'failed set_readonly': 1 });
      assert.deepStrictEqual(labels([setSend('json')]).outcomes, { 'failed set_not_property': 1 });
      for (const refused of ['status', 'ip', 'type', 'json']) assert.strictEqual(labels([setSend(refused)]).sets.size, 0);
    });

    it('wants the assigned value at argument 0 of a named member', () => {
      assert.deepStrictEqual(labels([setSend('body', { payload: { arg: 1 } })]).outcomes, { 'unchecked claim_invalid': 1 });
      assert.deepStrictEqual(labels([setSend('body', { payload: { arg: 0, key: 'x' } })]).outcomes, {
        'unchecked claim_invalid': 1,
      });
      assert.deepStrictEqual(labels([setSend(null)]).outcomes, { 'unchecked claim_invalid': 1 });
    });

    it('walks `path` to a property one object down', () => {
      const found = labels([setSend('body', { path: ['response'] })]);
      assert.deepStrictEqual(found.outcomes, { verified: 1 });
      const [key] = [...found.sets.keys()];
      assert.ok(read('fixture-ctx/index.d.ts', key).startsWith('body: unknown }'), key);
    });

    it('reads a property another package declares only through type_packages (characterisation)', () => {
      const claim = setSend('body', {}, 'RouterContext', 'fixture-ctx-router');
      assert.deepStrictEqual(labels([claim]).outcomes, { 'failed member_inherited': 1 });
      const named = labels([claim], { 'fixture-ctx-router': ['fixture-ctx'] });
      assert.deepStrictEqual(named.outcomes, { verified: 1 });
      assert.strictEqual(named.sets.size, 1);
    });

    it('leaves `set` on a receive, or on a message role, to a later slice', () => {
      const receive = check('fixture-routes', 'routes', 'export', {
        kind: 'op',
        op: 'receive',
        member: 'get',
        on: 'export',
        form: 'set',
        name: { arg: 0 },
        handler: { arg: 1 },
      });
      assert.strictEqual(verdict(receive), 'unchecked part_unbuilt');
      assert.strictEqual(verdict(setSend('body')), 'unchecked role_unsupported');
      const broker = check('fixture-ctx', 'Ctx', 'type', { kind: 'op', op: 'send', member: 'body', of: 'type', form: 'set', payload: { arg: 0 } }, 'broker');
      assert.strictEqual(verdict(broker), 'unchecked part_unbuilt');
    });
  });

  describe('new', () => {
    it('reads the construct signatures; a send under the server rules is a label keyed by them', () => {
      const found = labels([newSend('fixture-reply', 'Reply')]);
      assert.deepStrictEqual(found.outcomes, { verified: 1 });
      assert.strictEqual(found.sets.size, 0);
      const [key] = [...found.sends.keys()];
      assert.strictEqual(found.sends.size, 1);
      assert.ok(read('fixture-reply/index.d.ts', key).startsWith('constructor(body?: BodyLike | null, init?: Init);'), key);
    });

    it('refuses an export with no construct signature', () => {
      assert.deepStrictEqual(labels([newSend('fixture-reply', 'makeReply')]).outcomes, { 'failed member_not_constructible': 1 });
    });

    it('reads a static factory as the plain call send it is', () => {
      const found = labels([
        check('fixture-reply', 'AppReply', 'export', { kind: 'op', op: 'send', member: 'json', on: 'export', payload: { arg: 0 } }),
      ]);
      assert.deepStrictEqual(found.outcomes, { verified: 1 });
      assert.strictEqual(found.sends.size, 1);
    });

    it('reads a platform global named by a lib key', () => {
      const found = labels([newSend('lib:lib.dom.d.ts', 'Response')]);
      assert.deepStrictEqual(found.outcomes, { verified: 1 });
      assert.strictEqual(found.sends.size, 1);
    });

    it('reads a request that constructs, under the label v2 rules', () => {
      const request = check(
        'fixture-reply',
        'Fetcher',
        'export',
        { kind: 'op', op: 'request', member: null, on: 'export', form: 'new', name: { arg: 0 }, method_default: 'GET' },
        'http_client'
      );
      assert.strictEqual(verdict(request), 'verified');
    });

    it('is not a receive', () => {
      const receive = check('fixture-reply', 'Reply', 'export', {
        kind: 'op',
        op: 'receive',
        member: null,
        on: 'export',
        form: 'new',
        name: { arg: 0 },
        handler: { arg: 1 },
      });
      assert.strictEqual(verdict(receive), 'unchecked claim_invalid');
    });
  });

  describe('decorate', () => {
    it('holds when the call returns a method decorator, the verb being the export name', () => {
      assert.strictEqual(verdict(decorating('Get', { method: 'GET' })), 'verified');
      assert.strictEqual(verdict(decorating('Get', { method: 'POST' })), 'failed method_not_member_verb');
    });

    it('refuses an export whose call does not return a method decorator', () => {
      assert.strictEqual(verdict(decorating('Body')), 'failed decorate_not_decorator');
      assert.strictEqual(verdict(decorating('Controller')), 'failed decorate_not_decorator');
    });

    it('reads a class decorator scope whose name is a string', () => {
      const scope = (exported: string, extra: Record<string, unknown> = {}) =>
        check('fixture-decorators', exported, 'export', {
          kind: 'scope',
          member: exported,
          on: 'export',
          form: 'decorate',
          name: { arg: 0 },
          ...extra,
        });
      assert.strictEqual(verdict(scope('Controller')), 'verified');
      assert.strictEqual(verdict(scope('Body')), 'failed decorate_not_decorator');
      assert.strictEqual(verdict(scope('Get')), 'failed decorate_not_decorator');
      assert.strictEqual(verdict(scope('Controller', { name: undefined })), 'unchecked claim_invalid');
    });
  });

  describe('returns', () => {
    it('holds when the handler returns a type the route decides', () => {
      assert.strictEqual(verdict(returning('get', {}, { method: 'GET' })), 'verified');
    });

    it('is unchecked when the handler returns any', () => {
      assert.strictEqual(verdict(returning('use', {})), 'unchecked returns_untyped');
    });

    it('fails when the handler returns only void', () => {
      assert.strictEqual(verdict(returning('all', {})), 'failed returns_void');
    });

    it('needs a receive op with a handler', () => {
      const send = check('fixture-routes', 'routes', 'export', { kind: 'op', op: 'send', member: 'get', on: 'export', returns: {}, payload: { arg: 0 } });
      assert.deepStrictEqual(labels([send]).outcomes, { 'unchecked claim_invalid': 1 });
      assert.strictEqual(verdict(returning('get', {}, { handler: undefined })), 'unchecked claim_invalid');
    });

    it('checks `result` as a result claim is checked', () => {
      assert.strictEqual(verdict(returning('post', { result: { member: ['data'] } })), 'verified');
      assert.strictEqual(verdict(returning('post', { result: { member: ['items'] } })), 'failed result_not_candidate');
      assert.strictEqual(verdict(returning('post', { result: { whole: true } })), 'verified');
      // A return the route decides whole has no part to name.
      assert.strictEqual(verdict(returning('get', { result: { member: ['data'] } })), 'failed result_not_candidate');
      assert.strictEqual(verdict(returning('get', { result: { whole: true } })), 'verified');
    });

    it('adds nothing to the element: an op without `returns` answers as before', () => {
      assert.strictEqual(verdict(returning('all', undefined as unknown as Record<string, unknown>)), 'verified');
    });
  });
});
