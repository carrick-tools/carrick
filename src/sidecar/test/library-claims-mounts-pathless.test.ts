/**
 * Path-less mounts and pass-through members (K1a, the verifier half of the
 * koa-style mount spelling on carrick#2434).
 *
 * - `mounts: { arg, key?, handle: true }` is judged by the handler rule: the
 *   slot takes a declared function (a middleware), not the receiver itself.
 *   Without `handle` the instance rule is unchanged.
 * - A scope that binds (`mounts`, or `runs` with `yields`) may omit `name`.
 * - `passes: true` is a member whose value is its receiver, served at the
 *   receiver's own address: it stands alone, takes no string, and opens no
 *   `>scope:` receiver.
 * - `handle` on any slot but a scope's `mounts` is `claim_invalid`.
 *
 * The package is invented: a router whose `use` takes only middleware and
 * whose `routes()` hands back a middleware serving the router.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

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

const PACKAGE = `export type Ctx = { path: string };
export type Next = () => Promise<void>;
export type Middleware = (ctx: Ctx, next: Next) => Promise<void> | void;
export interface App {
  use(middleware: Middleware): this;
  register(plugin: (app: App) => void): this;
}
export declare function createApp(): App;
export declare class Router {
  use(middleware: Middleware): this;
  use(path: string, ...middleware: Middleware[]): this;
  post(path: string, handler: Middleware): this;
  routes(): Middleware;
  middleware(): Middleware;
  routesWith(): Middleware & { post(path: string, handler: Middleware): void };
  routesAt(path: string): Middleware;
  routesRest(...paths: string[]): Middleware;
  untyped(): any;
}
`;

interface Response {
  status: string;
  verdicts?: Array<{ claim_id: string; verdict: string; reason?: string }>;
  errors?: string[];
}

const PKG = 'fixture-mw';

const check = (id: string, exported: string, receiver: string, claim: Record<string, unknown>) => ({
  claim_id: id,
  package: PKG,
  export: exported,
  role: 'server_framework',
  receiver,
  claim,
});
const onRouter = (id: string, claim: Record<string, unknown>) => check(id, 'Router', 'instance:new', claim);
const onApp = (id: string, claim: Record<string, unknown>) => check(id, 'createApp', 'instance:()', claim);
const scope = (member: string, parts: Record<string, unknown>) => ({ kind: 'scope', member, ...parts });

describe('verify_library_claims: path-less mounts and pass-through members (K1a)', () => {
  let root: string;
  let client: SidecarClient;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-claims-mounts-pathless-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'service' }),
      'src/index.ts': 'export const service = 1;\n',
      [`node_modules/${PKG}/package.json`]: JSON.stringify({ name: PKG, version: '1.0.0', types: 'index.d.ts' }),
      [`node_modules/${PKG}/index.d.ts`]: PACKAGE,
    };
    for (const [rel, text] of Object.entries(files)) {
      const file = path.join(root, rel);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, text);
    }
    client = new SidecarClient();
    await client.start();
    const ready = await client.send<{ status: string }>({ request_id: 'init', action: 'init', repo_root: root });
    assert.strictEqual(ready.status, 'ready');
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  const verify = async (checks: unknown[]): Promise<Record<string, string>> => {
    const response = await client.send<Response>(
      {
        request_id: 'mounts-pathless',
        action: 'verify_library_claims',
        from_dir: root,
        checks: [
          check('make-router', 'Router', 'export', { kind: 'make', form: 'new', member: null }),
          check('make-app', 'createApp', 'export', { kind: 'make', form: 'call', member: null }),
          ...checks,
        ],
      },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    const read: Record<string, string> = {};
    for (const v of response.verdicts!) read[v.claim_id] = v.verdict === 'verified' ? 'verified' : `${v.verdict} ${v.reason}`;
    assert.strictEqual(read['make-router'], 'verified');
    assert.strictEqual(read['make-app'], 'verified');
    return read;
  };

  it('judges a handle mount by the handler rule, and an unflagged one by the instance rule', async () => {
    const read = await verify([
      onRouter('router-handle-pathless', scope('use', { mounts: { arg: 0, handle: true } })),
      onRouter('router-handle-named', scope('use', { name: { arg: 0 }, mounts: { arg: 1, handle: true } })),
      onRouter('router-plain-named', scope('use', { name: { arg: 0 }, mounts: { arg: 1 } })),
      onRouter('router-plain-pathless', scope('use', { mounts: { arg: 0 } })),
      onRouter('router-handle-false', scope('use', { name: { arg: 0 }, mounts: { arg: 1, handle: false } })),
      onApp('app-handle-pathless', scope('use', { mounts: { arg: 0, handle: true } })),
    ]);
    assert.deepStrictEqual(
      [
        read['router-handle-pathless'],
        read['router-handle-named'],
        read['router-plain-named'],
        read['router-plain-pathless'],
        read['router-handle-false'],
        read['app-handle-pathless'],
      ],
      [
        'verified', // use(middleware): a declared function
        'verified', // use(path, ...middleware): the rest element
        'failed mounts_not_instance', // the router is not a middleware
        'failed mounts_not_instance', // name-less, still the instance rule
        'failed mounts_not_instance', // handle: false is no flag
        'verified',
      ]
    );
  });

  it('lets a scope that binds omit its name, and refuses one that binds nothing', async () => {
    const yields = { param: 0, export: 'App', side: 'server' };
    const read = await verify([
      onApp('runs-pathless', scope('register', { runs: { arg: 0 }, yields })),
      onRouter('carries-nothing', scope('routes', {})),
      onRouter('passes-false', scope('routes', { passes: false })),
    ]);
    assert.deepStrictEqual(
      [read['runs-pathless'], read['carries-nothing'], read['passes-false']],
      ['verified', 'unchecked claim_invalid', 'unchecked claim_invalid']
    );
  });

  it('verifies a pass-through member that takes no string, alone', async () => {
    const read = await verify([
      onRouter('passes-routes', scope('routes', { passes: true })),
      onRouter('passes-middleware', scope('middleware', { passes: true })),
      onRouter('passes-at-path', scope('routesAt', { passes: true })),
      onRouter('passes-rest-paths', scope('routesRest', { passes: true })),
      onRouter('passes-untyped', scope('untyped', { passes: true })),
      onRouter('passes-named', scope('routes', { passes: true, name: { arg: 0 } })),
      onRouter('passes-mounts', scope('use', { passes: true, mounts: { arg: 0, handle: true } })),
      onApp('passes-runs', scope('register', { passes: true, runs: { arg: 0 }, yields: { param: 0, export: 'App', side: 'server' } })),
    ]);
    assert.deepStrictEqual(
      [
        read['passes-routes'],
        read['passes-middleware'],
        read['passes-at-path'],
        read['passes-rest-paths'],
        read['passes-untyped'],
        read['passes-named'],
        read['passes-mounts'],
        read['passes-runs'],
      ],
      [
        'verified',
        'verified',
        'failed name_ambiguous', // a path the claim gives no part
        'failed name_ambiguous',
        'unchecked scope_unresolved',
        'unchecked claim_invalid',
        'unchecked claim_invalid',
        'unchecked claim_invalid',
      ]
    );
  });

  it('opens no receiver on a pass-through member', async () => {
    const read = await verify([
      onRouter('passes-routes', scope('routesWith', { passes: true })),
      check('post-on-routes', 'Router', 'instance:new>scope:routesWith', {
        kind: 'op',
        op: 'receive',
        member: 'post',
        name: { arg: 0 },
        handler: { arg: 1 },
      }),
    ]);
    assert.strictEqual(read['passes-routes'], 'verified');
    // The member's return declares `post`; read as a scope it would verify.
    assert.strictEqual(read['post-on-routes'], 'unchecked scope_unresolved');
  });

  it('refuses handle on any slot but a scope mount', async () => {
    const yields = { param: 0, export: 'App', side: 'server' };
    const post = (handler: Record<string, unknown>) => ({ kind: 'op', op: 'receive', member: 'post', name: { arg: 0 }, handler });
    const read = await verify([
      onApp('handle-on-runs', scope('register', { runs: { arg: 0, handle: true }, yields })),
      onRouter('handle-on-name', scope('use', { name: { arg: 0, handle: true }, mounts: { arg: 1, handle: true } })),
      onRouter('handle-on-op', post({ arg: 1, handle: true })),
      onRouter('handle-false-on-op', post({ arg: 1, handle: false })),
    ]);
    assert.deepStrictEqual(
      [read['handle-on-runs'], read['handle-on-name'], read['handle-on-op'], read['handle-false-on-op']],
      ['unchecked claim_invalid', 'unchecked claim_invalid', 'unchecked claim_invalid', 'verified']
    );
  });
});
