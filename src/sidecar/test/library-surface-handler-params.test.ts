/**
 * carrick#2674 S3: `list_library_surface` with `handlers: true` (carrick#2966)
 * lists the members a route handler's own parameters declare, so a picker can
 * name `request.body` on a receive op's `request` field. They have a cap of their own, so the main listing, its
 * `truncated` count and the hash are the ones the lister gave without them.
 *
 * Every package below is invented and its declarations are hand-written.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as crypto from 'node:crypto';
import { SidecarClient } from './helpers.js';
import { fullSurfaceSha256 } from '../src/library-claims.js';
import type { LibrarySurface } from '../src/types.js';

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

const ROUTES = `export interface Request {
  body: unknown;
  params: Record<string, string>;
  headers: { host: string; accept?: string };
  raw: { url: string; method: string };
  get(name: string): string | undefined;
}
export interface Reply {
  send(body: unknown): void;
  status(code: number): this;
}
export type Handler = (request: Request, reply: Reply) => void;
export interface Router {
  get(path: string, handler: Handler): this;
  get(path: string, options: { schema?: object }, handler: Handler): this;
  route(options: { url: string; handler: Handler }): this;
  listen(port: number): void;
  on(event: string, listener: (payload: unknown) => void): this;
}
export declare function router(): Router;
`;

// Eight verbs that each take the same handler, so the handler members repeat
// across members and the cap bites before the signatures are listed.
const VERBS = ['get', 'post', 'put', 'patch', 'delete', 'head', 'options', 'all'];
const BIG = `export interface Req {
  body: unknown;
  query: { page?: string };
  raw: { url: string };
}
export type Handle = (req: Req) => void;
export interface Big {
${VERBS.map(verb => `  ${verb}(path: string, handler: Handle): this;`).join('\n')}
}
export declare function big(): Big;
`;

// A package with no function-taking member: it lists no handlers, so its
// hash is the one the lister gave before handlers were listed (taken from the
// lister of the commit before carrick#2674 S3).
const PLAIN = `export interface Store {
  get(key: string): string | undefined;
  set(key: string, value: string): this;
  size: number;
}
export declare function store(): Store;
`;
const PLAIN_SHA256 = 'fdcafa087f195f0aa200868c56992380a4439b554da8eb586881a3fa542b5497';

const OVER_CAP_SHA256 = '5a0c16b95fc38a8abb2ad3b6f20a2ebc3dccbbd66d02da6d942590a66c5c9982';

interface HandlerMember {
  path: string[];
  type: string;
  optional: boolean;
  function: boolean;
}
interface HandlerParam {
  param: number;
  name: string;
  type: string;
  members: HandlerMember[];
}
interface Handler {
  arg: number;
  key?: string;
  params: HandlerParam[];
}
interface Member {
  name: string;
  handlers?: Handler[];
  signatures: Array<{ params: unknown[] }>;
}
interface Surface {
  package: string;
  truncated: number;
  handlers_truncated?: number;
  exports: Array<{ export: string; receivers: Array<{ receiver: string; members: Member[] }> }>;
}

describe('list_library_surface: handler parameter members (carrick#2674 S3)', () => {
  let client: SidecarClient;
  let root: string;
  let requestId = 0;

  async function surface(packages: string[], extra: Record<string, unknown> = {}): Promise<Surface[]> {
    const response = await client.send<{ status: string; surfaces: Surface[]; errors?: string[] }>(
      { request_id: `s-${requestId++}`, action: 'list_library_surface', from_dir: root, packages, handlers: true, ...extra },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    return response.surfaces;
  }

  const membersOf = (listed: Surface): Member[] =>
    listed.exports.flatMap(e => e.receivers.flatMap(r => r.members));

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-surface-handlers-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'svc' }),
      'src/index.ts': 'export const service = 1;\n',
    };
    for (const [name, text] of [['fixture-routes', ROUTES], ['fixture-big', BIG], ['fixture-plain', PLAIN]]) {
      files[`node_modules/${name}/package.json`] = JSON.stringify({ name, version: '1.0.0', types: 'index.d.ts' });
      files[`node_modules/${name}/index.d.ts`] = text;
    }
    for (const [rel, text] of Object.entries(files)) {
      const file = path.join(root, rel);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, text);
    }
    client = new SidecarClient();
    await client.start();
    const init = await client.send<{ status: string }>({ request_id: 'init', action: 'init', repo_root: root });
    assert.strictEqual(init.status, 'ready');
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('lists the members of a handler`s parameters, positional and keyed, to two levels', async () => {
    const [routes] = await surface(['fixture-routes']);
    const byName = new Map(membersOf(routes).map(m => [m.name, m]));
    const get = byName.get('get')!;
    // The overloads put the handler at argument 1 and at argument 2: one each.
    assert.deepStrictEqual(get.handlers!.map(h => h.arg), [1, 2]);
    assert.deepStrictEqual(get.handlers![1].params, get.handlers![0].params);
    const [handler] = get.handlers!;
    assert.strictEqual(handler.arg, 1);
    assert.strictEqual(handler.key, undefined);
    const [request, reply] = handler.params;
    assert.deepStrictEqual([request.param, request.name, request.type], [0, 'request', 'Request']);
    assert.deepStrictEqual(
      request.members.map(m => [m.path.join('.'), m.type, m.optional, m.function]),
      [
        ['body', 'unknown', false, false],
        ['get', '(name: string) => string | undefined', false, true],
        ['headers', '{ host: string; accept?: string | undefined; }', false, false],
        ['headers.accept', 'string | undefined', true, false],
        ['headers.host', 'string', false, false],
        ['params', 'Record<string, string>', false, false],
        ['raw', '{ url: string; method: string; }', false, false],
        ['raw.method', 'string', false, false],
        ['raw.url', 'string', false, false],
      ]
    );
    assert.deepStrictEqual([reply.param, reply.name], [1, 'reply']);
    assert.deepStrictEqual(reply.members.map(m => m.path.join('.')), ['send', 'status']);

    // A handler held in a key of the options object is listed at that key.
    const route = byName.get('route')!;
    assert.deepStrictEqual(route.handlers!.map(h => [h.arg, h.key]), [[0, 'handler']]);
    assert.strictEqual(route.handlers![0].params[0].name, 'request');

    // A member that takes no function, and a callback whose parameter declares no member, list none.
    assert.strictEqual(byName.get('listen')!.handlers, undefined);
    assert.strictEqual(byName.get('on')!.handlers, undefined);
  });

  it('lists every handler member under a main cap that cuts the signatures', async () => {
    const [full] = await surface(['fixture-big']);
    const fullMembers = membersOf(full);
    assert.deepStrictEqual(fullMembers.map(m => m.name), [...VERBS].sort());
    assert.strictEqual(full.truncated, 0);
    assert.strictEqual(full.handlers_truncated, 0);
    const entries = (listed: Surface): number =>
      membersOf(listed).reduce(
        (n, m) => n + (m.handlers ?? []).reduce((k, h) => k + h.params.reduce((j, p) => j + 1 + p.members.length, 0), 0),
        0
      );
    const handlerEntries = entries(full);
    // Handler members take nothing from `max_entries` and are not cut by it
    // (their cap is per parameter, carrick#2966).
    const [capped] = await surface(['fixture-big'], { max_entries: 20 });
    assert.ok(capped.truncated > 0, 'nothing was cut');
    assert.ok(handlerEntries > 20, 'the fixture is too small to reach the cap');
    assert.strictEqual(entries(capped), handlerEntries);
  });

  it('hashes a package over the cap as the lister did before it listed handlers', async () => {
    // Taken from the lister of the commit before carrick#2674 S3, at the cap
    // below: handler members take nothing from the main cap, so the truncated
    // count and the hash are the ones it gave.
    const response = await client.send<{ status: string; surfaces: Surface[]; surface_sha256: string }>(
      { request_id: `s-${requestId++}`, action: 'list_library_surface', from_dir: root, handlers: true, packages: ['fixture-big'], max_entries: 20 },
      60_000
    );
    assert.strictEqual(response.surfaces[0].truncated, 6);
    assert.strictEqual(response.surface_sha256, OVER_CAP_SHA256);
  });

  it('keeps handlers out of the full-surface hash, at every depth', async () => {
    const response = await client.send<{ status: string; surfaces: Surface[]; surface_sha256: string }>(
      { request_id: `s-${requestId++}`, action: 'list_library_surface', from_dir: root, handlers: true, packages: ['fixture-routes'] },
      60_000
    );
    const [routes] = response.surfaces;
    assert.ok(membersOf(routes).some(m => m.handlers !== undefined), 'the fixture lists no handlers');
    const stripped = JSON.parse(JSON.stringify(routes, (key, value) => (key === 'handlers' || key === 'result_candidates' ? undefined : value))) as Surface;
    const expected = crypto
      .createHash('sha256')
      .update(JSON.stringify([[routes.package, stripped.exports]]))
      .digest('hex');
    assert.strictEqual(response.surface_sha256, expected);
    const withHandlers = crypto
      .createHash('sha256')
      .update(JSON.stringify([[routes.package, routes.exports]]))
      .digest('hex');
    assert.notStrictEqual(response.surface_sha256, withHandlers);
  });

  it('hashes a package with no handlers as the lister did before it listed them', async () => {
    const response = await client.send<{ status: string; surfaces: Surface[]; surface_sha256: string }>(
      { request_id: `s-${requestId++}`, action: 'list_library_surface', from_dir: root, handlers: true, packages: ['fixture-plain'] },
      60_000
    );
    assert.ok(membersOf(response.surfaces[0]).every(m => m.handlers === undefined));
    assert.strictEqual(response.surface_sha256, PLAIN_SHA256);
  });

  it('hashes a surface the same with and without handlers', () => {
    const member = (handlers?: unknown) => ({ name: 'get', own: true, ...(handlers ? { handlers } : {}), signatures: [] });
    const surface = (members: unknown[]): LibrarySurface =>
      ({ package: 'p', exports: [{ export: 'x', receivers: [{ receiver: 'r', members }] }] }) as unknown as LibrarySurface;
    const handlers = [{ arg: 1, params: [{ param: 0, name: 'request', type: 'R', members: [] }] }];
    assert.strictEqual(fullSurfaceSha256([surface([member(handlers)])]), fullSurfaceSha256([surface([member()])]));
    // Another field of the member still moves the hash.
    assert.notStrictEqual(
      fullSurfaceSha256([surface([{ ...member(), own: false }])]),
      fullSurfaceSha256([surface([member()])])
    );
  });
});
