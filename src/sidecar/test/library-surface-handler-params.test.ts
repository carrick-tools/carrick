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

// carrick#2977 test 5, the middleware shape: a module exported whole, whose
// namespace holds the context alias every middleware is handed.
const MIDDLEWARE = `declare function App(): App.Application;
declare namespace App {
  interface Base {
    request: { body: unknown };
    set(name: string, value: string): void;
  }
  type Context<S = {}> = Base & { state: S; send(body: unknown): void };
  interface Application {
    use(mw: (ctx: App.Context, next: () => Promise<void>) => unknown): this;
    listen(port: number): void;
  }
}
export = App;
`;

// carrick#2977 test 6: handed types no entry may name. An anonymous object; a
// type-only export with an undefaulted type parameter; a value export (a
// class); a type the specifier re-exports from a package outside the handing
// receiver's home; and a handed type that shares its name with an export of
// the specifier but is another declaration (the express shape: the export
// extends the handed type).
const CORE = `export interface Response {
  send(body: unknown): void;
  locals: Record<string, unknown>;
}
export interface Request {
  body: unknown;
}
`;
const UNNAMED = `import * as core from 'fixture-core';
export { Request } from 'fixture-core';
export interface Response extends core.Response {
  extra: string;
}
export interface Gen<T> {
  value: T;
  send(body: T): void;
}
export declare class Conn {
  send(body: unknown): void;
}
export interface App {
  anon(handler: (ctx: { body: unknown; send(body: unknown): void }) => void): this;
  anon2(handler: (ctx: { body: unknown; send(body: unknown): void }) => void): this;
  gen(handler: (g: Gen<string>) => void): this;
  conn(handler: (c: Conn) => void): this;
  get(path: string, handler: (req: core.Request, res: core.Response) => void): this;
}
export declare function app(): App;
`;

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
  type_id: number;
}
interface HandlerType {
  id: number;
  type: string;
  members: HandlerMember[];
  dropped: number;
  export?: string;
}
/** A parameter as the lister wrote it before the table (carrick#2977). */
interface InlineParam {
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
interface Receiver {
  receiver: string;
  members: Member[];
}
interface Surface {
  package: string;
  truncated: number;
  handler_types?: HandlerType[];
  types?: Array<{ export: string; receivers: Receiver[] }>;
  handler_params_left_out?: number;
  exports: Array<{ export: string; receivers: Receiver[] }>;
}

/** Each parameter's entry, by its `type_id`. */
const entryOf = (listed: Surface, param: HandlerParam): HandlerType => {
  const entry = listed.handler_types?.find(each => each.id === param.type_id);
  assert.ok(entry, `type_id ${param.type_id} has no entry`);
  return entry;
};

/** A parameter resolved to the inline shape the lister wrote before the table. */
const inline = (listed: Surface, param: HandlerParam): InlineParam => {
  const entry = entryOf(listed, param);
  return { param: param.param, name: param.name, type: entry.type, members: entry.members };
};

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
    for (const [name, text] of [
      ['fixture-routes', ROUTES],
      ['fixture-big', BIG],
      ['fixture-plain', PLAIN],
      ['fixture-middleware', MIDDLEWARE],
      ['fixture-core', CORE],
      ['fixture-unnamed', UNNAMED],
    ]) {
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
    const [request, reply] = handler.params.map(param => inline(routes, param));
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
    assert.strictEqual(full.handler_params_left_out, 0);
    const entries = (listed: Surface): number =>
      membersOf(listed).reduce(
        (n, m) =>
          n + (m.handlers ?? []).reduce((k, h) => k + h.params.reduce((j, p) => j + 1 + entryOf(listed, p).members.length, 0), 0),
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
    const handlers = [{ arg: 1, params: [{ param: 0, name: 'request', type_id: 1 }] }];
    assert.strictEqual(fullSurfaceSha256([surface([member(handlers)])]), fullSurfaceSha256([surface([member()])]));
    // Another field of the member still moves the hash.
    assert.notStrictEqual(
      fullSurfaceSha256([surface([{ ...member(), own: false }])]),
      fullSurfaceSha256([surface([member()])])
    );
  });

  // carrick#2977 ticket A, test 1.
  it('lists each handed type once, referred to by id from every parameter and overload', async () => {
    const [routes] = await surface(['fixture-routes']);
    assert.deepStrictEqual(routes.handler_types!.map(e => [e.id, e.type]), [[1, 'Request'], [2, 'Reply']]);
    const params = membersOf(routes).flatMap(m => (m.handlers ?? []).flatMap(h => h.params));
    // get at argument 1 and at argument 2 (two overloads), route at key `handler`: two parameters each.
    assert.deepStrictEqual(
      params.map(p => [p.name, p.type_id]),
      [['request', 1], ['reply', 2], ['request', 1], ['reply', 2], ['request', 1], ['reply', 2]]
    );
    // A parameter carries only its slot, name and id.
    for (const param of params) assert.deepStrictEqual(Object.keys(param), ['param', 'name', 'type_id']);
  });

  // carrick#2977 ticket A, test 2: hard-coded from the lister of e8136c0e, before the table.
  it('resolves every id to exactly the type and members the inline lister gave', async () => {
    const [routes] = await surface(['fixture-routes']);
    const get = membersOf(routes).find(m => m.name === 'get')!;
    assert.deepStrictEqual(get.handlers![0].params.map(p => inline(routes, p))[1], {
      param: 1,
      name: 'reply',
      type: 'Reply',
      members: [
        { path: ['send'], type: '(body: unknown) => void', optional: false, function: true },
        { path: ['status'], type: '(code: number) => Reply', optional: false, function: true },
      ],
    });
    const [big] = await surface(['fixture-big']);
    for (const member of membersOf(big)) {
      assert.deepStrictEqual(inline(big, member.handlers![0].params[0]), {
        param: 0,
        name: 'req',
        type: 'Req',
        members: [
          { path: ['body'], type: 'unknown', optional: false, function: false },
          { path: ['query'], type: '{ page?: string | undefined; }', optional: false, function: false },
          { path: ['query', 'page'], type: 'string | undefined', optional: true, function: false },
          { path: ['raw'], type: '{ url: string; }', optional: false, function: false },
          { path: ['raw', 'url'], type: 'string', optional: false, function: false },
        ],
      });
    }
    assert.strictEqual(big.handler_types!.length, 1);
  });

  // carrick#2977 ticket A, test 3 (the cap's own drop count is in the opt-in suite).
  it('counts the cap`s drop once per entry', async () => {
    const [big] = await surface(['fixture-big']);
    assert.deepStrictEqual(big.handler_types!.map(e => e.dropped), [0]);
    assert.ok(!('handlers_truncated' in big), 'the per-parameter drop count is still written');
  });

  // carrick#2977 ticket A, test 4.
  it('names a type-only export a handler is handed, and lists it as receiver `type`', async () => {
    const [routes] = await surface(['fixture-routes']);
    assert.deepStrictEqual(routes.handler_types!.map(e => [e.type, e.export]), [['Request', 'Request'], ['Reply', 'Reply']]);
    assert.deepStrictEqual(routes.types!.map(t => t.export), ['Reply', 'Request']);
    const [reply] = routes.types!;
    assert.deepStrictEqual(reply.receivers.map(r => r.receiver), ['type']);
    const send = reply.receivers[0].members.find(m => m.name === 'send')!;
    assert.deepStrictEqual(send.signatures.map(sig => sig.params.length), [1]);
    assert.deepStrictEqual(reply.receivers[0].members.map(m => m.name), ['send', 'status']);
    // A `type` receiver lists no handlers and no scope receiver.
    assert.ok(!JSON.stringify(routes.types).includes('"handlers"'));
    assert.ok(!JSON.stringify(routes.types).includes('"scope_receiver"'));
  });

  it('names a handed type export whatever value exports the request asks for', async () => {
    const [routes] = await surface(['fixture-routes'], { exports: { 'fixture-routes': ['router'] } });
    assert.deepStrictEqual(routes.exports.map(e => e.export), ['router']);
    assert.deepStrictEqual(routes.handler_types!.map(e => e.export), ['Request', 'Reply']);
    assert.deepStrictEqual(routes.types!.map(t => t.export), ['Reply', 'Request']);
  });

  // carrick#2977 ticket A, test 5.
  it('names a context alias inside the namespace of a module exported whole', async () => {
    const [middleware] = await surface(['fixture-middleware']);
    const use = membersOf(middleware).find(m => m.name === 'use')!;
    const ctx = use.handlers![0].params.find(p => p.name === 'ctx')!;
    assert.strictEqual(entryOf(middleware, ctx).export, 'Context');
    assert.deepStrictEqual(middleware.types!.map(t => t.export), ['Context']);
    assert.deepStrictEqual(middleware.types![0].receivers[0].members.map(m => m.name), ['send', 'set']);
  });

  // carrick#2977 ticket A, test 6.
  it('names no anonymous object, undefaulted generic, value export, foreign declaration or same-name other type', async () => {
    const [unnamed] = await surface(['fixture-unnamed']);
    const byName = new Map(membersOf(unnamed).map(m => [m.name, m]));
    for (const name of ['anon', 'gen', 'conn', 'get']) {
      const handlers = byName.get(name)!.handlers;
      assert.ok(handlers, `${name} lists no handler`);
      for (const param of handlers[0].params) {
        assert.strictEqual(entryOf(unnamed, param).export, undefined, `${name} ${param.name}`);
      }
    }
    assert.deepStrictEqual(byName.get('get')!.handlers![0].params.map(p => p.name), ['req', 'res']);
    // Two anonymous types written alike are two type objects with one content: one entry.
    assert.strictEqual(byName.get('anon2')!.handlers![0].params[0].type_id, byName.get('anon')!.handlers![0].params[0].type_id);
    assert.deepStrictEqual(unnamed.types, []);
  });

  // carrick#2977 ticket A, test 7.
  it('writes none of the table keys with handlers off', async () => {
    const response = await client.send<{ status: string; surfaces: Surface[] }>(
      { request_id: `s-${requestId++}`, action: 'list_library_surface', from_dir: root, packages: ['fixture-routes', 'fixture-middleware'] },
      60_000
    );
    for (const listed of response.surfaces) {
      assert.deepStrictEqual(
        Object.keys(listed).filter(key => key !== 'resolved_file' && key !== 'installed_version'),
        ['package', 'truncated', 'exports']
      );
    }
  });
});
