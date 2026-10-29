/**
 * carrick#2674: the request and reply bodies a route handler's own parameters
 * state, read through the members the service's server-framework labels name.
 *
 * The server package is invented, from the general shape of two styles of
 * server library:
 * - a keyed route generic: `delete<P extends Parts = Parts>(path, (req:
 *   Req<P>, rep: Rep<P>) => unknown)`, where the request's body member is
 *   `unknown` until the route's type argument fills it, and the reply's send
 *   method takes the route's reply part;
 * - positional request and reply type parameters: `Request2<Params, ResBody,
 *   ReqBody>` and `Response2<ResBody>`, whose send member is a property typed
 *   by a call-signature alias. Beside it sit a send member with a fixed
 *   `(path: string)` payload and one typed by its own type parameter.
 *
 * The labels reach `infer` as the store answers them: a receive op whose
 * `request.body` names the body member, and `send` ops with a payload slot.
 * A member is matched by its declaration, never by its name.
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

const KIT = `export interface Parts { Body?: unknown; Params?: unknown; Reply?: unknown }
export type Part<P, K extends string> = P extends { [key in K]: infer X } ? X : unknown;
export interface Req<P extends Parts = Parts> {
  body: Part<P, 'Body'>;
  params: Part<P, 'Params'>;
  headers: Record<string, string>;
  raw: { pipe(to: unknown): void };
  file(): Promise<unknown>;
}
export interface Rep<P extends Parts = Parts> {
  send(payload?: Part<P, 'Reply'>): Rep<P>;
  sendFile(path: string): Rep<P>;
  status(code: number): Rep<P>;
}
export interface App {
  get<P extends Parts = Parts>(path: string, handler: (req: Req<P>, rep: Rep<P>) => unknown): App;
  post<P extends Parts = Parts>(path: string, handler: (req: Req<P>, rep: Rep<P>) => unknown): App;
  delete<P extends Parts = Parts>(path: string, handler: (req: Req<P>, rep: Rep<P>) => unknown): App;
}
declare function make(): App;
export default make;
export declare const reply: Rep;

import type { CoreRequest, CoreResponse } from 'route-kit-core';
export interface Request2<Params = any, ResBody = any, ReqBody = any> extends CoreRequest<Params, ReqBody> {}
export interface Response2<ResBody = any> extends CoreResponse<ResBody> {}
export type Handler2 = (req: Request2, res: Response2) => void;
export interface Router {
  post(path: string, ...handlers: Handler2[]): Router;
}
export declare function router(): Router;
export declare const response: Response2;
`;

const APP = `import make from 'route-kit';
import { router, type Req, type Request2, type Response2 } from 'route-kit';

interface NewItem { name: string }
interface Item { id: string; name: string }
interface Ok { id: string; total: number }
interface Err { error: string; code: number }
interface NewOrder { lines: string[] }
interface OrderContract { Params: { shop: string }; Reply: Ok | Err; Body: NewOrder }
interface Legacy { Params: {}; Body: never; Reply: Item }
interface Bare { Params: { id: string }; Body: never; Reply: Item }

declare function save(item: NewItem): Item;
declare function create(order: NewOrder): Promise<Ok>;
declare function createLoosely(order: NewOrder): Promise<any>;
declare function problem(): any;

function audit(request: Req): void {
  console.log(request.headers['x-actor']);
}
function parse(request: Req): unknown {
  return request.body;
}
function parseLoosely(request: { body: unknown }): unknown {
  return request.body;
}
function respond(res: Response2<Ok | Err>, value: Ok): void {
  res.status(201).json(value);
}
export function wrap<T extends { Params?: unknown; Reply?: unknown; Body?: unknown }>(
  fn: (req: Request2<T['Params'], T['Reply'], T['Body']>, res: Response2<T['Reply']>) => Promise<void>
) {
  return fn;
}
function auth(req: Request2, res: Response2, next: () => void): void {
  next();
}
function pair(first: Req, second: Req): void {
  console.log(first.headers['x-actor'], second.body);
}
declare function getRawBody(source: unknown): Promise<string>;
declare function guard(): (req: Request2, res: Response2, next: () => void) => void;
declare const sink: unknown;

const app = make();
const r = router();

// A
app.delete<{ Params: { id: string } }>('/items/:id', async (req, rep) => { audit(req); rep.send(); });
// B
app.post<{ Params: { id: string } }>('/items/:id/run', async (req, rep) => { parse(req); rep.send(); });
// C
app.post('/raw', async (req, rep) => { audit(req); rep.send(); });
// D
app.post<{ Body: NewItem; Reply: Item }>('/items', async (req, rep) => { rep.send(save(req.body)); });
// E
r.post('/orders', auth, createOrder);
// F
r.post('/orders-helper', auth, createOrderByHelper);
// G
r.post('/legacy', (req: Request2, res: Response2) => { res.json(problem()); });
// H
app.post('/own-type', async (req: OwnRequest, rep: OwnReply<Item>) => { audit(req); rep.send(); });
// I
app.delete<{ Params: { id: string } }>('/items/:id/upload', async (req, rep) => { await req.file(); rep.send(); });
// J
r.post('/forward', wrap<Legacy>(async (req, res) => { forward(req.body); res.json(problem()); }));
// K
app.post<{ Params: { id: string } }>('/items/:id/loose', async (req, rep) => { parseLoosely(req); rep.send(); });
// L
r.post('/orders-loose', auth, createOrderLoosely);
// M
app.post<{ Params: { id: string }; Reply: { id: string; meta: any } }>('/meta', async (req, rep) => { rep.send(); });
// O
app.delete<{ Params: { id: string } }>('/items/:id/stream', async (req, rep) => { req.raw.pipe(sink); rep.send(); });
// P
app.delete<{ Params: { id: string } }>('/items/:id/raw', async (req, rep) => { await getRawBody(req.raw); rep.send(); });
// Q
app.delete<{ Params: { id: string } }>('/items/:id/pair', async (req, rep) => { pair(req, req); rep.send(); });
// S
r.post('/bare', guard(), wrap<Bare>(async (req, res) => { res.json(save({ name: 'x' })); }));
// T
r.post('/bare-open', wrap<Bare>(async (req, res) => { res.json(save({ name: 'x' })); }));
// U
app.get<{ Reply: 'on' | 'off' }>('/state', async (req, rep) => { rep.send('on'); });

declare function forward(body: unknown): void;
interface OwnRequest { body: NewItem; headers: Record<string, string> }
interface OwnReply<R> { send(payload?: R): void }

export const createOrder = wrap<OrderContract>(async (req, res) => {
  if (req.body.lines.length === 0) {
    res.status(400).json({ error: 'empty', code: 1 });
    return;
  }
  const created = await create(req.body);
  res.download('/receipts/latest');
  res.sendCode(201);
  res.echo(created);
  res.status(201).json(created);
});

export const createOrderLoosely = wrap<OrderContract>(async (req, res) => {
  if (req.body.lines.length === 0) {
    res.status(400).json({ error: 'empty', code: 1 });
    return;
  }
  res.status(201).json(await createLoosely(req.body));
});

export const createOrderByHelper = wrap<OrderContract>(async (req, res) => {
  if (req.body.lines.length === 0) {
    res.status(400).json({ error: 'empty', code: 1 });
    return;
  }
  respond(res, await create(req.body));
});
`;

// The positional style's request and reply members live in a type-only
// package the server package re-exports through its own types, as the store
// lists it in `type_packages`.
const CORE = `export type Send<B, T> = (body?: B) => T;
export interface CoreRequest<Params = any, ReqBody = any> {
  body: ReqBody;
  params: Params;
}
export interface CoreResponse<ResBody = any, Code extends number = number> {
  json: Send<ResBody, this>;
  sendCode(code: Code): this;
  download(path: string): this;
  echo<T>(body: ResBody | T): this;
  status(code: number): this;
}
`;

const PKG = 'route-kit';
const CORE_PKG = 'route-kit-core';
let sequence = 0;
type Claim = Record<string, unknown> & { kind: string };
const check = (exported: string, receiver: string, claim: Claim) => ({
  claim_id: `s${sequence++}`,
  package: PKG,
  export: exported,
  role: 'server_framework',
  receiver,
  claim,
});
const receive = (exported: string, member: string) =>
  check(exported, 'instance:()', {
    kind: 'op',
    op: 'receive',
    member,
    of: 'instance:()',
    name: { arg: 0 },
    handler: { arg: 1 },
    method: member.toUpperCase(),
    request: { param: 0, body: ['body'] },
  });
const send = (exported: string, member: string) =>
  check(exported, 'export', { kind: 'op', op: 'send', member, on: 'export', payload: { arg: 0 } });

/** The labels as the store answers them for the package. */
const LABELS = [
  check('default', 'export', { kind: 'make', form: 'call', member: null }),
  check('router', 'export', { kind: 'make', form: 'call', member: null }),
  receive('default', 'post'),
  receive('default', 'delete'),
  receive('router', 'post'),
  send('reply', 'send'),
  send('reply', 'sendFile'),
  send('response', 'json'),
  send('response', 'download'),
  send('response', 'echo'),
  send('response', 'sendCode'),
];

/**
 * The same labels with the reply's sends placed on the type export `Rep`
 * itself (`of: "type"`, carrick#2977 B), not on the value `reply` the kit
 * declares to work around the gap; and with no reply send at all.
 */
const typeSend = (member: string) =>
  check('Rep', 'type', { kind: 'op', op: 'send', member, of: 'type', payload: { arg: 0 } });
const NO_REPLY_SENDS = LABELS.filter((each) => each.export !== 'reply');
const TYPE_LABELS = [...NO_REPLY_SENDS, typeSend('send'), typeSend('sendFile')];

interface Inferred {
  alias: string;
  type_string: string;
  any_provenance?: Array<{ path: string; reason: string }>;
}

describe('carrick#2674: bodies a route handler’s parameters state', () => {
  let root: string;
  let appPath: string;
  let client: SidecarClient;
  let id = 0;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-stated-params-'));
    fs.writeFileSync(path.join(root, 'tsconfig.json'), TSCONFIG);
    fs.mkdirSync(path.join(root, 'node_modules', PKG), { recursive: true });
    fs.writeFileSync(
      path.join(root, 'node_modules', PKG, 'package.json'),
      JSON.stringify({ name: PKG, version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(root, 'node_modules', PKG, 'index.d.ts'), KIT);
    fs.mkdirSync(path.join(root, 'node_modules', CORE_PKG), { recursive: true });
    fs.writeFileSync(
      path.join(root, 'node_modules', CORE_PKG, 'package.json'),
      JSON.stringify({ name: CORE_PKG, version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(root, 'node_modules', CORE_PKG, 'index.d.ts'), CORE);
    fs.mkdirSync(path.join(root, 'src'));
    appPath = path.join(root, 'src', 'app.ts');
    fs.writeFileSync(appPath, APP);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: root });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  /** The registration line under the `// <label>` comment. */
  const lineOf = (label: string): number => APP.split('\n').indexOf(`// ${label}`) + 2;

  const infer = async (
    label: string,
    kind: 'request_body' | 'response_body',
    labels = true,
    typePackages: Record<string, string[]> = { [PKG]: [CORE_PKG] },
    checks: unknown[] = LABELS
  ): Promise<Inferred | undefined> => {
    const alias = `${label}_${kind}_${labels}_${Object.keys(typePackages).length}_${id}`;
    const response = await client.send<{ inferred_types?: Inferred[] }>(
      {
        action: 'infer',
        request_id: `infer-${id++}`,
        requests: [{ file_path: appPath, line_number: lineOf(label), infer_kind: kind, alias }],
        ...(labels ? { server_claims: { from_dir: root, checks, type_packages: typePackages } } : {}),
      },
      60_000
    );
    return response.inferred_types?.find((t) => t.alias === alias);
  };

  /** An answer without its alias, which names whether the request carried labels. */
  const unaliased = (inferred: Inferred | undefined) => {
    if (!inferred) return undefined;
    const { alias: _alias, ...rest } = inferred;
    return rest;
  };
  /** The answer with labels equals the answer without them. */
  const unchanged = async (label: string, kind: 'request_body' | 'response_body') =>
    assert.deepStrictEqual(unaliased(await infer(label, kind)), unaliased(await infer(label, kind, false)), label);

  const reason = (inferred: Inferred | undefined) =>
    inferred?.any_provenance?.find((p) => p.path === '')?.reason;

  it('A: a route whose type argument states its parts and no body, and whose helpers read none, takes no request body', async () => {
    const inferred = await infer('A', 'request_body');
    assert.strictEqual(inferred?.type_string, 'unknown');
    assert.strictEqual(reason(inferred), 'no_request_body');
  });

  it('B, C, I, J and K: a body read, no written type argument, a request member called, a never body forwarded, or a helper typed without the labelled member decide nothing', async () => {
    for (const label of ['B', 'C', 'I', 'J', 'K']) {
      const inferred = await infer(label, 'request_body');
      assert.notStrictEqual(reason(inferred), 'no_request_body', label);
    }
  });

  it('D: the route’s parts fill the request and reply parameters', async () => {
    assert.match((await infer('D', 'request_body'))?.type_string ?? '', /name: string/);
    const reply = await infer('D', 'response_body');
    assert.match(reply?.type_string ?? '', /id: string/);
  });

  it('E: a stated reply that unions success and error bodies is cut to the success send', async () => {
    const reply = await infer('E', 'response_body');
    assert.ok(reply, 'E answers a reply');
    assert.match(reply.type_string, /total: number/);
    assert.doesNotMatch(reply.type_string, /error: string/);
    assert.match((await infer('E', 'request_body'))?.type_string ?? '', /lines: string\[\]/);
  });

  it('D and E: a send labelled on the reply`s type export reads as the one on a value export does (carrick#2977 B)', async () => {
    for (const [label, kind] of [
      ['D', 'response_body'],
      ['E', 'response_body'],
      ['E', 'request_body'],
    ] as const) {
      const byValue = unaliased(await infer(label, kind));
      const byType = unaliased(await infer(label, kind, true, { [PKG]: [CORE_PKG] }, TYPE_LABELS));
      assert.deepStrictEqual(byType, byValue, `${label} ${kind}`);
    }
    // Without a reply send in either set, D's reply is not read from its send.
    const unsent = await infer('D', 'response_body', true, { [PKG]: [CORE_PKG] }, NO_REPLY_SENDS);
    assert.doesNotMatch(unsent?.type_string ?? '', /id: string/);
  });

  it('E: a reply member a type-only package declares is read only where the store lists that package', async () => {
    const reply = await infer('E', 'response_body', true, {});
    assert.doesNotMatch(reply?.type_string ?? '', /total: number/);
  });

  it('F and L: with no typed success send through the reply parameter, the union is not published', async () => {
    for (const label of ['F', 'L']) {
      const reply = await infer(label, 'response_body');
      assert.doesNotMatch(reply?.type_string ?? '', /total: number/, label);
      await unchanged(label, 'response_body');
    }
  });

  it('G: a reply parameter whose payload defaults to any states nothing new', async () => {
    await unchanged('G', 'response_body');
  });

  it('H: members the repo declares under the labelled names state nothing', async () => {
    const request = await infer('H', 'request_body');
    assert.notStrictEqual(reason(request), 'no_request_body');
    await unchanged('H', 'response_body');
  });

  it('M: a stated reply holding any or unknown is not published; the reading without labels stands', async () => {
    await unchanged('M', 'response_body');
  });

  it('O, P and Q: a method called on a request member, a member handed to a function, or the request handed at a second position decide nothing', async () => {
    for (const label of ['O', 'P', 'Q']) {
      assert.notStrictEqual(reason(await infer(label, 'request_body')), 'no_request_body', label);
    }
  });

  it('S and T: a route with another function handed in beside its handler decides nothing; without one it takes no body', async () => {
    assert.notStrictEqual(reason(await infer('S', 'request_body')), 'no_request_body');
    assert.strictEqual(reason(await infer('T', 'request_body')), 'no_request_body');
  });

  it('U: a stated reply through an optional payload slot does not publish undefined', async () => {
    const reply = await infer('U', 'response_body');
    assert.match(reply?.type_string ?? '', /"on"/);
    assert.doesNotMatch(reply?.type_string ?? '', /undefined/);
  });

  it('a claim the schema refuses is dropped; the others still read', async () => {
    const refused = { ...LABELS[0], claim: { kind: 'op', op: 'teleport' } };
    assert.match((await infer('D', 'request_body', true, { [PKG]: [CORE_PKG] }, [refused, ...LABELS]))?.type_string ?? '', /name: string/);
    assert.match((await infer('D', 'response_body', true, { [PKG]: [CORE_PKG] }, [refused, ...LABELS]))?.type_string ?? '', /id: string/);
  });

  it('with no labels in the request, nothing reads a stated parameter', async () => {
    assert.notStrictEqual(reason(await infer('A', 'request_body', false)), 'no_request_body');
    assert.doesNotMatch((await infer('E', 'response_body', false))?.type_string ?? '', /total: number/);
  });
});
