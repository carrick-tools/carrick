/**
 * carrick#1913 and carrick#1912: a route whose handler is a named function
 * declared in another file.
 *
 * The registration file holds a name and nothing else, so a model that reads
 * it locates no response and no body. The handler's file holds the contract
 * and no registration. What the route answers with is in one of three places:
 *
 *  - the handler RETURNS it (the payload, or a send that carries it);
 *  - the handler SENDS it through a parameter it was handed, and returns
 *    nothing;
 *  - the handler is bound to a call that wraps the function which does one of
 *    those (`const handler = wrap<T>(async (req, res) => ...)`).
 *
 * Two requests are read here. One is located AT the handler: a
 * `response_body` or `request_body` whose span is the handler's own
 * declaration, in the handler's own file. The other is located at the
 * registration, which is what a scan sends for every other row; three answers
 * it gave there were wrong (the path literal, a named middleware in place of
 * the handler, and an unanswered request the capture then filled with the
 * router's own type).
 *
 * "Sends through a parameter" is read from types and structure only: a method
 * called directly on a parameter whose type is transport the repo does not
 * declare, whose own declaration leaves its first parameter open, given a
 * value that reads as a payload, as the last thing the handler does with its
 * parameters on that path. No method name is read, which the fixture shows by
 * giving the response type a header setter and a destroyer with the same
 * shape of call.
 *
 * The router, its request and its response come from an invented package
 * under `node_modules`, so the origin gate sees an installed dependency as it
 * does in a real tree.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const PACKAGE_DTS = `export interface Req<B = unknown> {
  body: B;
  params: Record<string, string>;
}

export interface Res<B = unknown> {
  statusCode: number;
  locals: { audit: { record(entry: unknown): void } };
  status(code: number): this;
  setHeader(name: string, value: string): this;
  getHeader(name: string): string | undefined;
  /** A header object: an open first parameter that is not the body. */
  set(fields: unknown): this;
  json(body?: B): this;
  send(body?: unknown): this;
  write(chunk: unknown): boolean;
  end(): void;
  /** A first parameter the library fixes: not a body slot. */
  destroy(error?: Error): this;
}

export type Next = (error?: unknown) => void;
export type Handler = (req: Req<any>, res: Res<any>, next: Next) => unknown;

export interface Router {
  get(path: string, ...handlers: Handler[]): this;
  post(path: string, ...handlers: Handler[]): this;
  put(path: string, ...handlers: Handler[]): this;
  delete(path: string, ...handlers: Handler[]): this;
}
export function createRouter(): Router;

export interface Ctx {
  req: { raw: Request };
}
export interface Engine {
  handle(request: Request): Response | Promise<Response>;
}
export function createEngine(): Engine;
`;

const WIDGETS_TS = `export interface Widget {
  id: string;
  name: string;
  parts: number;
}

export interface NewWidget {
  name: string;
  parts: number;
}

export async function listAll(): Promise<Widget[]> {
  return [];
}

export async function store(input: NewWidget): Promise<Widget> {
  return { id: 'w1', ...input };
}
`;

const HANDLERS_TS = `import type { Ctx, Next, Req, Res } from 'wirekit';
import { createEngine } from 'wirekit';
import { listAll, store, type NewWidget, type Widget } from './widgets';

export async function listWidgets(_req: Req, _res: Res): Promise<Widget[]> {
  return listAll();
}

export const showWidget = async (req: Req, res: Res) => {
  const widgets = await listAll();
  return res.json({ widget: widgets[0], asked: req.params.id });
};

export const createWidget = async (req: Req<NewWidget>, res: Res) => {
  const created = await store(req.body);
  res.status(201).json(created);
};

export const removeWidget = async (_req: Req, res: Res) => {
  res.status(204).end();
};

export const auditWidget = async (req: Req, res: Res, next: Next) => {
  try {
    const widgets = await listAll();
    res.json({ audited: widgets.length, by: req.params.id });
  } catch (error) {
    return next(error);
  }
};

export const requireKey = (_req: Req, _res: Res, next: Next) => next();

function guarded<B>(handler: (req: Req<B>, res: Res) => Promise<void>) {
  return (req: Req<B>, res: Res, next: Next) => handler(req, res).catch(next);
}

export const renameWidget = guarded<NewWidget>(async (req, res) => {
  const renamed = await store(req.body);
  res.json(renamed);
});

interface RequestWithCtx extends Request {
  ctx: Ctx;
}

const engine = createEngine();

export function handleQuery(c: Ctx): Response | Promise<Response> {
  const request = c.req.raw as RequestWithCtx;
  request.ctx = c;
  return engine.handle(request);
}
`;

// One handler per decision the reading makes, named for the shape it holds.
// The sidecar is asked by span, so the names carry no meaning to it.
const SENDS_TS = `import type { Next, Req, Res } from 'wirekit';
import { listAll, store, type NewWidget, type Widget } from './widgets';

declare function failWith(res: Res, error: unknown): void;
declare function respondWith(res: Res, body: unknown): void;
declare const reason: number;

export const missingThenFound = async (req: Req, res: Res) => {
  const widgets = await listAll();
  if (widgets.length === 0) {
    res.status(404).json({ error: 'no widget', asked: req.params.id });
    return;
  }
  res.json(widgets[0]);
};

export const returnsTheMissing = async (req: Req, res: Res) => {
  const widgets = await listAll();
  if (widgets.length === 0) return res.status(404).json({ error: 'no widget' });
  res.json(widgets[0]);
};

export const statusSetFirst = async (_req: Req, res: Res) => {
  const widgets = await listAll();
  if (widgets.length === 0) {
    res.statusCode = 404;
    res.json({ error: 'no widget' });
    return;
  }
  res.json({ count: widgets.length });
};

export const statusNotWritten = async (_req: Req, res: Res) => {
  res.status(reason).json({ note: 'depends' });
};

export const statusNotWrittenBesideOne = async (_req: Req, res: Res) => {
  const widgets = await listAll();
  if (widgets.length === 0) {
    res.status(reason).json({ note: 'depends' });
    return;
  }
  res.json({ count: widgets.length });
};

export const headersThenBody = async (_req: Req, res: Res) => {
  res.set({ 'cache-control': 'no-store' });
  res.json({ fresh: true });
};

export const headersInTheChain = async (_req: Req, res: Res) => {
  res.set({ 'cache-control': 'no-store' }).status(200).json({ chained: true });
};

export const writesThenEnds = async (_req: Req, res: Res) => {
  res.write(JSON.stringify({ part: 1 }));
  res.end();
};

export const sendsText = async (_req: Req, res: Res) => {
  res.send('accepted');
};

export const destroys = async (_req: Req, res: Res) => {
  res.destroy(new Error('gone'));
};

export const passesOn = (_req: Req, _res: Res, next: Next) => {
  next({ status: 400, message: 'not here' });
};

export const recordsThenEnds = async (req: Req, res: Res) => {
  res.locals.audit.record({ seen: req.params.id });
  res.status(204).end();
};

export const recordsThenSends = async (req: Req, res: Res) => {
  res.locals.audit.record({ seen: req.params.id });
  res.json({ recorded: true });
};

export const handsOff = async (_req: Req, res: Res) => {
  const widgets = await listAll();
  respondWith(res, widgets);
};

export const handsOffTheFailure = async (_req: Req, res: Res) => {
  try {
    res.json({ stored: await store({ name: 'n', parts: 1 }) });
  } catch (error) {
    failWith(res, error);
  }
};

export const sendsInACallback = (_req: Req, res: Res) => {
  listAll().then((widgets) => res.json(widgets));
};

export const sendsOrReturns = async (req: Req, res: Res) => {
  const widgets = await listAll();
  if (req.params.id === 'all') {
    res.json(widgets);
    return;
  }
  return widgets[0];
};

export const usedAfterTheSend = async (_req: Req, res: Res) => {
  res.json({ early: true });
  res.setHeader('x-late', 'yes');
};

export const assignsTheBody = async (_req: Req, res: Res) => {
  (res as Res & { body: unknown }).body = { assigned: true };
};

/** A response type the repo declares itself, with the same members. */
interface OwnReply {
  statusCode: number;
  setHeader(name: string, value: string): this;
  getHeader(name: string): string | undefined;
  json(body?: unknown): this;
}

export const sendsOnItsOwnType = async (_req: Req, reply: OwnReply) => {
  reply.json({ owned: true });
};

/** A repo type that extends the library's response is still transport. */
interface TimedRes extends Res<Widget> {
  startedAt: number;
}

export const sendsOnASubtype = async (_req: Req, res: TimedRes) => {
  const widgets = await listAll();
  res.json(widgets[0]);
};

type Quoting = (req: Req) => Promise<{ total: number }>;
declare function consumed(handler: Quoting): (req: Req, res: Res) => Promise<void>;
declare function passedThrough<F>(handler: F): F;

export const quoteConsumed = consumed(async (_req) => ({ total: 3 }));

export const quotePassedThrough = passedThrough(async (_req: Req) => ({ total: 3 }));

export const storesTyped = async (req: Req<NewWidget>, res: Res) => {
  res.status(201).json(await store(req.body));
};

export const branches = async (req: Req, res: Res) => {
  switch (req.params.id) {
    case 'first':
      res.json({ first: true });
      break;
    default:
      if (req.params.id.length > 3) {
        res.json({ long: true });
      } else {
        res.json({ short: true });
      }
  }
};

export const writesInALoop = async (_req: Req, res: Res) => {
  for (const widget of await listAll()) {
    res.write(widget);
  }
  res.end();
};

export const conciseSend = (_req: Req, res: Res) => res.json({ concise: true });

declare function rejectWith(res: Res, code: string): void;
declare function finishWith(options: { res: Res; widgets: Widget[] }): Promise<void>;

function rejects(res: Res, code: string): void {
  res.status(400).json({ error: code });
}

function answersAll(res: Res, widgets: Widget[]): void {
  res.json({ widgets });
}

function passesAlong(res: Res): void {
  rejects(res, 'again');
}

async function finishes({ res, widgets }: { res: Res; widgets: Widget[] }): Promise<void> {
  res.status(200).json({ finished: widgets.length });
}

export const guardsWithAHelper = async (req: Req, res: Res) => {
  const widgets = await listAll();
  if (widgets.length === 0) {
    rejects(res, 'no_widget');
    return;
  }
  res.status(200).json({ found: widgets[0], asked: req.params.id });
};

export const answersThroughAHelper = async (req: Req, res: Res) => {
  const widgets = await listAll();
  if (req.params.id === 'all') {
    answersAll(res, widgets);
    return;
  }
  res.json({ one: widgets[0] });
};

export const handsOnTwice = async (_req: Req, res: Res) => {
  passesAlong(res);
};

export const onlyRejects = async (_req: Req, res: Res) => {
  rejects(res, 'never');
};

export const finishesInAnObject = async (req: Req, res: Res) => {
  const widgets = await listAll();
  if (req.params.id === 'none') {
    res.status(400).json({ error: 'bad id' });
    return;
  }
  await finishes({ res, widgets });
};
declare function record(entry: { status: number; bytes: string | undefined }): void;
declare function later(run: () => void): void;

export const guardsThenSends = async (req: Req, res: Res) => {
  const widgets = await listAll();
  if (widgets.length === 0) {
    rejectWith(res, 'no_widget');
    return;
  }
  res.status(200).json({ found: widgets[0], asked: req.params.id });
};

export const handsOffInAnObject = async (req: Req, res: Res) => {
  const widgets = await listAll();
  if (req.params.id === 'none') {
    res.status(400).json({ error: 'bad id' });
    return;
  }
  await finishWith({ res, widgets });
};

export const readsAfterTheSend = async (_req: Req, res: Res) => {
  res.json({ measured: true });
  record({ status: res.statusCode, bytes: res.getHeader('content-length') });
};

export const readsInACallback = async (_req: Req, res: Res) => {
  later(() => record({ status: res.statusCode, bytes: res.getHeader('content-length') }));
  res.json({ logged: true });
};

declare function allowed(request: Request): Promise<boolean>;

export async function platformHandler(request: Request): Promise<Response> {
  if (!(await allowed(request))) {
    return Response.json({ error: 'not allowed' }, { status: 403 });
  }
  await request.text();
  return Response.json({ platform: true });
}

export const sendsThenReturnsIt = async (_req: Req, res: Res) => {
  res.json({ returned: true });
  return res;
};

declare function toLabel(widget: Widget): { label: string };

export const mapsWithANamedFunction = async (_req: Req, res: Res) => {
  const widgets = await listAll();
  res.json(widgets.map(toLabel));
};

interface Tagged {
  id: string;
  tags: unknown;
}
declare function loadTagged(): Promise<Tagged>;

export const sendsAnOpenMember = async (_req: Req, res: Res) => {
  res.json(await loadTagged());
};
`;

const ROUTES_TS = `import { createRouter } from 'wirekit';
import {
  auditWidget,
  createWidget,
  listWidgets,
  removeWidget,
  renameWidget,
  requireKey,
  showWidget,
} from './handlers';

export const router = createRouter();

router.get('/widgets', listWidgets);
router.get('/widgets/:id', requireKey, showWidget);
router.post('/widgets', requireKey, createWidget);
router.delete('/widgets/:id', removeWidget);
router.get('/widgets/:id/audit', auditWidget);
router.post('/widgets/:id/name', renameWidget);
router.put('/widgets/:id', requireKey, renameWidget);

router.get(
  '/widgets/inline',
  requireKey,
  async (_req, res) => {
    res.json({ inline: true });
  }
);
`;

interface Inferred {
  alias: string;
  type_string: string;
  is_explicit: boolean;
  any_provenance?: Array<{ path: string; kind: string; reason: string; detail?: string }>;
}

interface InferShape {
  inferred_types?: Inferred[];
  errors?: string[];
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

const WIDGET = '{ id: string; name: string; parts: number; }';
const NEW_WIDGET = '{ name: string; parts: number; }';

/** The 1-based line of the first line of `source` that holds `marker`. */
function lineOf(source: string, marker: string): number {
  const index = source.split('\n').findIndex((line) => line.includes(marker));
  assert.ok(index >= 0, `fixture has no line holding ${JSON.stringify(marker)}`);
  return index + 1;
}

/**
 * The span of the declaration `source` opens with `opening`, as the scanner
 * states a handler: from the binding's name (or the function's first keyword
 * after \`export\`) to the end of the statement, without its semicolon.
 */
function declarationSpan(source: string, opening: string): { start: number; end: number } {
  const at = source.indexOf(opening);
  assert.ok(at >= 0, `fixture has no declaration opening ${JSON.stringify(opening)}`);
  // A declaration written on one line ends at its own semicolon.
  const firstLine = source.slice(at, source.indexOf('\n', at)).trimEnd();
  if (firstLine.endsWith(';')) {
    return { start: at, end: at + firstLine.length - 1 };
  }
  const close = source.indexOf('\n}', at);
  assert.ok(close >= 0, `the declaration opening ${JSON.stringify(opening)} never closes`);
  // `\n}` then an optional `)` for a call that wraps the function.
  let end = close + 2;
  if (source[end] === ')') end += 1;
  return { start: at, end };
}

describe('carrick#1913: a route whose handler is a named function in another file', () => {
  let client: SidecarClient;
  let repoDir: string;
  const fileOf = (name: string): string => path.join(repoDir, 'src', name);

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1913-'));
    const packageDir = path.join(repoDir, 'node_modules', 'wirekit');
    fs.mkdirSync(packageDir, { recursive: true });
    fs.writeFileSync(
      path.join(packageDir, 'package.json'),
      JSON.stringify({ name: 'wirekit', version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(packageDir, 'index.d.ts'), PACKAGE_DTS);
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'commonjs',
          moduleResolution: 'node',
          target: 'es2022',
          lib: ['es2022', 'dom'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(fileOf('widgets.ts'), WIDGETS_TS);
    fs.writeFileSync(fileOf('handlers.ts'), HANDLERS_TS);
    fs.writeFileSync(fileOf('sends.ts'), SENDS_TS);
    fs.writeFileSync(fileOf('routes.ts'), ROUTES_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function ask(item: Record<string, unknown>): Promise<Inferred | undefined> {
    const alias = `Alias_${Math.random().toString(36).slice(2)}`;
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [{ ...item, alias }],
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  /** A request located at the handler: its own file, its own span. */
  function atHandler(file: string, source: string, opening: string, kind = 'response_body') {
    const span = declarationSpan(source, opening);
    return ask({
      file_path: fileOf(file),
      line_number: lineOf(source, opening),
      infer_kind: kind,
      span_start: span.start,
      span_end: span.end,
    });
  }

  const inHandlers = (opening: string, kind?: string) =>
    atHandler('handlers.ts', HANDLERS_TS, opening, kind);
  const inSends = (opening: string, kind?: string) =>
    atHandler('sends.ts', SENDS_TS, opening, kind);

  /** A request located at the registration, by its line or by its call's span. */
  function atRegistration(marker: string, kind: string, by: 'line' | 'span') {
    const line = lineOf(ROUTES_TS, marker);
    if (by === 'line') {
      return ask({ file_path: fileOf('routes.ts'), line_number: line, infer_kind: kind });
    }
    const start = ROUTES_TS.indexOf(marker);
    const end = ROUTES_TS.indexOf(');', start) + 1;
    return ask({
      file_path: fileOf('routes.ts'),
      line_number: line,
      infer_kind: kind,
      span_start: start,
      span_end: end,
    });
  }

  function assertBody(inferred: Inferred | undefined, expected: string, why: string): void {
    assert.ok(inferred, `${why}: the request must be answered`);
    assert.strictEqual(collapse(inferred.type_string), expected, why);
  }

  /**
   * `unknown` with a root reason, so the capture does not run the locator
   * again and print the function or the router in its place.
   */
  function assertDecided(inferred: Inferred | undefined, reason: string, why: string): void {
    assert.ok(inferred, `${why}: the request must be answered, or the capture asks again`);
    assert.strictEqual(collapse(inferred.type_string), 'unknown', why);
    const root = (inferred.any_provenance ?? []).find((entry) => entry.path === '');
    assert.ok(root, `${why}: the answer must carry a root reason`);
    assert.strictEqual(root.reason, reason, why);
    assert.strictEqual(root.kind, 'unknown');
    assert.ok(root.detail && root.detail.length > 0, 'the reason carries a sentence');
    assert.ok(!/\/(tmp|var|Users|private)\//.test(root.detail), 'the sentence holds no path');
  }

  describe('a response request located at the handler', () => {
    it('reads what a handler returns', async () => {
      assertBody(
        await inHandlers('async function listWidgets'),
        `${WIDGET}[]`,
        'the declared return is the body'
      );
    });

    it('reads the body of a send the handler returns', async () => {
      assertBody(
        await inHandlers('showWidget = async'),
        `{ widget: ${WIDGET}; asked: string; }`,
        'a returned send carries its argument'
      );
    });

    it('reads the body a handler sends through its response parameter and returns nothing', async () => {
      assertBody(
        await inHandlers('createWidget = async'),
        WIDGET,
        'the value handed to the send is the body, and void is not'
      );
    });

    it('answers a handler that sends no body with a reason of its own', async () => {
      assertDecided(
        await inHandlers('removeWidget = async'),
        'no_response_body',
        'a send that takes no value sends no body'
      );
    });

    it('reads the send beside a continuation call returned in a catch', async () => {
      assertBody(
        await inHandlers('auditWidget = async'),
        '{ audited: number; by: string; }',
        'the catch clause answers a failure, and the continuation is not a send'
      );
    });

    it('follows a handler bound to a wrapper call to the function it wraps', async () => {
      assertBody(
        await inHandlers('renameWidget = guarded'),
        WIDGET,
        'the wrapped function sends the body'
      );
    });

    it('never reads the request handed to a call that returns transport as the body', async () => {
      const inferred = await inHandlers('function handleQuery');
      assertDecided(
        inferred,
        'handler_body_unread',
        'a repo type that extends the platform request is the request'
      );
    });

    it('does not publish that request for a return-value row asked at the line either', async () => {
      const inferred = await ask({
        file_path: fileOf('handlers.ts'),
        line_number: lineOf(HANDLERS_TS, 'function handleQuery'),
        infer_kind: 'function_return',
      });
      assert.ok(
        !inferred || /^(any|unknown)$/.test(collapse(inferred.type_string)),
        `the request object is not the response, got: ${inferred?.type_string}`
      );
    });

    it('never prints the function, the router or void as a body', async () => {
      const openings = [
        'async function listWidgets',
        'showWidget = async',
        'createWidget = async',
        'removeWidget = async',
        'auditWidget = async',
        'requireKey = (',
        'renameWidget = guarded',
        'function handleQuery',
      ];
      for (const opening of openings) {
        const inferred = await inHandlers(opening);
        assert.ok(inferred, `${opening}: a request at a handler is always answered`);
        const text = collapse(inferred.type_string);
        assert.ok(!text.includes('=>'), `${opening}: a callable is not a body, got ${text}`);
        assert.ok(!/\bRouter\b/.test(text), `${opening}: the router is not a body`);
        assert.notStrictEqual(text, 'void', `${opening}: void is no answer`);
      }
    });

    it('does not read a function that returns a function as a handler', async () => {
      const inferred = await inHandlers('function guarded<B>');
      assertDecided(
        inferred,
        'handler_body_unread',
        'a function that builds a handler states no body of its own'
      );
    });

    it('reads a handler from the line it opens on as it does from its span', async () => {
      const byLine = (opening: string) =>
        ask({
          file_path: fileOf('handlers.ts'),
          line_number: lineOf(HANDLERS_TS, opening),
          infer_kind: 'response_body',
        });
      assertBody(await byLine('createWidget = async'), WIDGET, 'void is not the body');
      assertDecided(
        await byLine('removeWidget = async'),
        'no_response_body',
        'nor is it the answer for a handler that sends none'
      );
      assertBody(await byLine('renameWidget = guarded'), WIDGET, 'through the wrapper call too');
    });

    it('reads the same handler from a span that opens at the export keyword', async () => {
      const from = HANDLERS_TS.indexOf('export const createWidget');
      const span = declarationSpan(HANDLERS_TS, 'createWidget = async');
      const inferred = await ask({
        file_path: fileOf('handlers.ts'),
        line_number: lineOf(HANDLERS_TS, 'createWidget = async'),
        infer_kind: 'response_body',
        span_start: from,
        span_end: span.end + 1,
      });
      assertBody(inferred, WIDGET, 'the statement that declares the handler names it too');
    });

    it('reads the same handler from the span of the function alone', async () => {
      const from = HANDLERS_TS.indexOf('async (req: Req<NewWidget>, res: Res) =>');
      const span = declarationSpan(HANDLERS_TS, 'createWidget = async');
      const inferred = await ask({
        file_path: fileOf('handlers.ts'),
        line_number: lineOf(HANDLERS_TS, 'createWidget = async'),
        infer_kind: 'response_body',
        span_start: from,
        span_end: span.end,
      });
      assertBody(inferred, WIDGET, 'the function is the handler');
    });
  });

  describe('what counts as a send through a parameter', () => {
    it('leaves out a send whose chain states an error status', async () => {
      assertBody(
        await inSends('missingThenFound = async'),
        WIDGET,
        'the 404 body is not the success body'
      );
    });

    it('leaves out an error send the handler returns beside a send it does not', async () => {
      assertBody(
        await inSends('returnsTheMissing = async'),
        WIDGET,
        'a returned send and a plain one are both sends on the parameter'
      );
    });

    it('reads a status the statement before the send assigns', async () => {
      assertBody(
        await inSends('statusSetFirst = async'),
        '{ count: number; }',
        'the number assigned to a member of the parameter states the status'
      );
    });

    it('abstains on a path whose status the source does not write', async () => {
      assertDecided(
        await inSends('statusNotWritten = async'),
        'handler_body_unread',
        'a status that is not a written number makes the path abstain'
      );
    });

    it('does not join a path whose status the source does not write', async () => {
      assertBody(
        await inSends('statusNotWrittenBesideOne = async'),
        '{ count: number; }',
        'the unread path stays out of the join'
      );
    });

    it('does not read a header object set before the send as the body', async () => {
      assertBody(
        await inSends('headersThenBody = async'),
        '{ fresh: boolean; }',
        'only the last call on the parameter is the send'
      );
    });

    it('reads the last call of a chain, not the calls before it', async () => {
      assertBody(
        await inSends('headersInTheChain = async'),
        '{ chained: boolean; }',
        'earlier calls of the chain configure the send'
      );
    });

    it('does not call a written chunk followed by an end "no body"', async () => {
      assertDecided(
        await inSends('writesThenEnds = async'),
        'handler_body_unread',
        'a value went into an open slot before the response ended'
      );
    });

    it('abstains on a primitive body', async () => {
      assertDecided(
        await inSends('sendsText = async'),
        'handler_body_unread',
        'a string is not a contract a JSON reader can be checked against'
      );
    });

    it('does not read a value handed to a parameter the library fixes', async () => {
      assertDecided(
        await inSends('destroys = async'),
        'handler_body_unread',
        'a slot the library types is not a body slot'
      );
    });

    it('does not read a call OF a parameter as a send', async () => {
      assertDecided(
        await inSends('passesOn = ('),
        'handler_body_unread',
        'a continuation handed a typed error sends nothing'
      );
    });

    it('does not read a call on a member of the parameter as a send', async () => {
      assertDecided(
        await inSends('recordsThenEnds = async'),
        'no_response_body',
        'a record written to a member object is not the response'
      );
      assertBody(
        await inSends('recordsThenSends = async'),
        '{ recorded: boolean; }',
        'the send on the parameter itself is'
      );
    });

    it('joins the sends of every branch that ends in one', async () => {
      assertBody(
        await inSends('branches = async'),
        '{ first: boolean; } | { long: boolean; } | { short: boolean; }',
        'a break leaves the switch and not the handler, and nothing follows the switch'
      );
    });

    it('does not read a write inside a loop as the last send', async () => {
      assertDecided(
        await inSends('writesInALoop = async'),
        'handler_body_unread',
        'a loop runs its body again'
      );
    });

    it('reads a send that is the whole body of the handler', async () => {
      assertBody(
        await inSends('conciseSend = ('),
        '{ concise: boolean; }',
        'a concise body returns its one expression'
      );
    });

    it('abstains when the parameter is handed to another function last', async () => {
      assertDecided(
        await inSends('handsOff = async'),
        'handler_body_unread',
        'what that function sends is not in this handler'
      );
    });

    it('never drops a path it hands on to a function it cannot read', async () => {
      // The helper has no body in the program. Guessing that it reports an
      // error would publish a body that is missing whatever it does send.
      const inferred = await inSends('guardsThenSends = async');
      assertDecided(
        inferred,
        'handler_body_unread',
        'the handler sends a body itself, and a helper nobody can read sends on another path'
      );
      assert.match((inferred?.any_provenance ?? [])[0]?.detail ?? '', /no body in the program/);
    });

    it('follows a hand-off into an error helper and leaves that path out', async () => {
      assertBody(
        await inSends('guardsWithAHelper = async'),
        `{ found: ${WIDGET}; asked: string; }`,
        'the helper states a written 400, so its path is an error path'
      );
    });

    it('joins the body a helper sends on the path that hands on to it', async () => {
      assertBody(
        await inSends('answersThroughAHelper = async'),
        `{ widgets: ${WIDGET}[]; } | { one: ${WIDGET}; }`,
        'the helper sends a success body, and it is part of what the route sends'
      );
    });

    it('follows a hand-off one level and no further', async () => {
      const inferred = await inSends('handsOnTwice = async');
      assertDecided(
        inferred,
        'handler_body_unread',
        'the helper hands the transport on again'
      );
      assert.match((inferred?.any_provenance ?? [])[0]?.detail ?? '', /hands it on again/);
    });

    it('says only errors when the one helper a handler hands on to only rejects', async () => {
      assertDecided(
        await inSends('onlyRejects = async'),
        'no_success_payload',
        'the path was read, and it states an error'
      );
    });

    it('follows a hand-off made inside an object to the member it arrives as', async () => {
      assertBody(
        await inSends('finishesInAnObject = async'),
        '{ finished: number; }',
        'the helper takes the object apart in its parameter list'
      );
    });

    it('reads a parameter passed inside an object as handed on, and says so', async () => {
      const inferred = await inSends('handsOffInAnObject = async');
      assertDecided(
        inferred,
        'handler_body_unread',
        'the only send of its own is an error; the success is in the function it hands on to'
      );
      const detail = (inferred?.any_provenance ?? [])[0]?.detail ?? '';
      assert.match(detail, /another function/, 'the reason is the hand-off, not "only errors"');
    });

    it('does not take a read of the parameter after the send for another send', async () => {
      assertBody(
        await inSends('readsAfterTheSend = async'),
        '{ measured: boolean; }',
        'a call whose value is used reads the response, it does not send one'
      );
    });

    it('does not abstain over a callback that only reads the parameter', async () => {
      assertBody(
        await inSends('readsInACallback = async'),
        '{ logged: boolean; }',
        'nothing in the callback sends'
      );
    });

    it('leaves out a hand-off in a catch clause', async () => {
      assertBody(
        await inSends('handsOffTheFailure = async'),
        `{ stored: ${WIDGET}; }`,
        'a catch clause answers a failure'
      );
    });

    it('abstains when the send is inside a callback the handler passes on', async () => {
      assertDecided(
        await inSends('sendsInACallback = ('),
        'handler_body_unread',
        'the handler itself sends nothing'
      );
    });

    it('abstains when a handler sends on one path and returns its payload on another', async () => {
      assertDecided(
        await inSends('sendsOrReturns = async'),
        'handler_body_unread',
        'which of the two the route answers with is not stated'
      );
    });

    it('abstains when the parameter is used again after the send', async () => {
      assertDecided(
        await inSends('usedAfterTheSend = async'),
        'handler_body_unread',
        'the send was not the last thing done with the parameter'
      );
    });

    it('abstains when the body is set by assignment', async () => {
      assertDecided(
        await inSends('assignsTheBody = async'),
        'handler_body_unread',
        'an assignment is not read tonight'
      );
    });

    it('does not read a request as something a handler sends through', async () => {
      // The platform request is transport, and nothing can be sent through
      // it: no method of it takes a body. The handler answers by what it
      // returns, whatever it does with the request on the way.
      assertBody(
        await inSends('async function platformHandler'),
        '{ platform: boolean; }',
        'handing the request to a helper, or reading it, sends nothing'
      );
    });

    it('reads a send whose transport the handler then returns', async () => {
      assertBody(
        await inSends('sendsThenReturnsIt = async'),
        '{ returned: boolean; }',
        'returning the parameter hands it back and sends nothing more'
      );
    });

    it('does not read a send on a response type the repo declares', async () => {
      assertDecided(
        await inSends('sendsOnItsOwnType = async'),
        'handler_body_unread',
        'the handler was not handed that type by a library'
      );
    });

    it('reads a send on a repo type that extends the transport', async () => {
      assertBody(
        await inSends('sendsOnASubtype = async'),
        WIDGET,
        'the send is the one the library declares'
      );
    });
  });

  describe('what stays as it was', () => {
    it('reads a located call that is handed a named function as the payload it is', async () => {
      // A model located this call by its text. It is the body, not a
      // registration: the function it is handed maps the rows.
      const inferred = await ask({
        file_path: fileOf('sends.ts'),
        line_number: lineOf(SENDS_TS, 'mapsWithANamedFunction = async'),
        infer_kind: 'response_body',
        expression_text: 'widgets.map(toLabel)',
        expression_line: lineOf(SENDS_TS, 'res.json(widgets.map(toLabel))'),
      });
      assertBody(inferred, '{ label: string; }[]', 'the mapped list is what the route sends');
    });

    it('reads the same list from a request at the handler', async () => {
      assertBody(
        await inSends('mapsWithANamedFunction = async'),
        '{ label: string; }[]',
        'the send is handed the mapped list'
      );
    });

    it('answers a body that holds an open member with unknown, not with the function', async () => {
      // The scanner does not carry a text that holds `unknown`; it runs the
      // request's locator through the capture, and this locator is a function.
      assertDecided(
        await inSends('sendsAnOpenMember = async'),
        'handler_body_unread',
        'an unanswered request at a handler prints the handler'
      );
    });
  });

  describe('a handler bound to a wrapper call', () => {
    it('does not publish what the wrapped function returns when the wrapper consumes it', async () => {
      assertDecided(
        await inSends('quoteConsumed = consumed'),
        'handler_body_unread',
        'the bound handler returns nothing, so what is sent is the wrapper’s to decide'
      );
    });

    it('reads what the wrapped function returns when the wrapper returns the same', async () => {
      assertBody(
        await inSends('quotePassedThrough = passedThrough'),
        '{ total: number; }',
        'the bound handler returns what the function it wraps returns'
      );
    });

    it('reads the request body off the function the call wraps', async () => {
      assertBody(
        await inHandlers('renameWidget = guarded', 'request_body'),
        NEW_WIDGET,
        'the type argument of the wrapper types the wrapped request'
      );
    });

    it('reads the request body a plain handler declares', async () => {
      assertBody(
        await inSends('storesTyped = async', 'request_body'),
        NEW_WIDGET,
        'the parameter states the body'
      );
    });

    it('answers a request at a handler that reads no typed body, without printing it', async () => {
      assertDecided(
        await inHandlers('removeWidget = async', 'request_body'),
        'handler_body_unread',
        'nothing in the handler states a body'
      );
    });
  });

  describe('a request located at the registration', () => {
    it('follows a named handler from the span of the registration, not the path literal', async () => {
      assertBody(
        await atRegistration("router.get('/widgets', listWidgets", 'response_body', 'span'),
        `${WIDGET}[]`,
        'the path is not the response'
      );
      const sent = await atRegistration("router.delete('/widgets/:id'", 'response_body', 'span');
      assertDecided(sent, 'no_response_body', 'the handler sends no body');
    });

    it('answers a return-value row on a named handler from the line alone', async () => {
      assertBody(
        await atRegistration("router.get('/widgets', listWidgets", 'function_return', 'line'),
        `${WIDGET}[]`,
        'the handler the line registers returns the body'
      );
      assertBody(
        await atRegistration("router.get('/widgets/:id/audit'", 'function_return', 'line'),
        '{ audited: number; by: string; }',
        'a handler that returns nothing is read for what it sends'
      );
    });

    it('leaves a function_return at a registration whose handler is inline and lines below as it was', async () => {
      // The fix above is for a handler declared somewhere else. An inline one
      // the line search does not reach is unanswered, as it was.
      const line = ROUTES_TS.split('\n').findIndex((text) => text === 'router.get(') + 1;
      assert.ok(line > 0, 'the fixture registers one route over several lines');
      const inferred = await ask({
        file_path: fileOf('routes.ts'),
        line_number: line,
        infer_kind: 'function_return',
      });
      assert.strictEqual(inferred, undefined, `expected no answer, got ${inferred?.type_string}`);
    });

    it('reads the handler, not the named middleware in front of it', async () => {
      assertBody(
        await atRegistration("router.get('/widgets/:id', requireKey", 'response_body', 'line'),
        `{ widget: ${WIDGET}; asked: string; }`,
        'the last function the registration is handed answers the route'
      );
      assertBody(
        await atRegistration("router.post('/widgets', requireKey", 'response_body', 'span'),
        WIDGET,
        'the handler behind the middleware sends the body'
      );
    });

    it('reads the request body of the handler behind a named middleware', async () => {
      assertBody(
        await atRegistration("router.post('/widgets', requireKey", 'request_body', 'span'),
        NEW_WIDGET,
        'the middleware declares no body, the handler does'
      );
      assertBody(
        await atRegistration("router.post('/widgets', requireKey", 'request_body', 'line'),
        NEW_WIDGET,
        'by line as by span'
      );
    });

    it('follows a wrapper-bound handler for the request and the response', async () => {
      assertBody(
        await atRegistration("router.post('/widgets/:id/name'", 'request_body', 'span'),
        NEW_WIDGET,
        'the registration call itself is not the request'
      );
      assertBody(
        await atRegistration("router.post('/widgets/:id/name'", 'response_body', 'span'),
        WIDGET,
        'the wrapped function sends the body'
      );
      assertBody(
        await atRegistration("router.put('/widgets/:id', requireKey", 'response_body', 'line'),
        WIDGET,
        'behind a middleware as well'
      );
    });

    it("reads the function a registration names from the span of that name", async () => {
      const nameSpan = (marker: string, name: string) => {
        const start = ROUTES_TS.indexOf(name, ROUTES_TS.indexOf(marker));
        return ask({
          file_path: fileOf('routes.ts'),
          line_number: lineOf(ROUTES_TS, marker),
          infer_kind: 'response_body',
          span_start: start,
          span_end: start + name.length,
        });
      };
      assertBody(
        await nameSpan("router.post('/widgets', requireKey", 'createWidget'),
        WIDGET,
        'the name says which function'
      );
      assertDecided(
        await nameSpan("router.delete('/widgets/:id'", 'removeWidget'),
        'no_response_body',
        'and it is answered where the function sends nothing'
      );
      assertBody(
        await nameSpan("router.post('/widgets/:id/name'", 'renameWidget'),
        WIDGET,
        'a name bound to a wrapper call is followed'
      );
      const middleware = await nameSpan("router.post('/widgets', requireKey", 'requireKey');
      assert.ok(middleware, 'answered');
      assert.ok(
        !collapse(middleware.type_string).includes('=>'),
        `a name is never answered with the function it names, got ${middleware.type_string}`
      );
    });

    it('never answers a registration with its own value', async () => {
      const markers = [
        "router.get('/widgets', listWidgets",
        "router.get('/widgets/:id', requireKey",
        "router.post('/widgets', requireKey",
        "router.delete('/widgets/:id'",
        "router.get('/widgets/:id/audit'",
        "router.post('/widgets/:id/name'",
        "router.put('/widgets/:id', requireKey",
      ];
      for (const marker of markers) {
        for (const by of ['line', 'span'] as const) {
          const inferred = await atRegistration(marker, 'response_body', by);
          assert.ok(inferred, `${marker} by ${by}: answered, so the capture does not ask again`);
          const text = collapse(inferred.type_string);
          assert.ok(!text.startsWith('"/'), `${marker} by ${by}: the path literal, got ${text}`);
          assert.ok(!/\bRouter\b/.test(text), `${marker} by ${by}: the router, got ${text}`);
          assert.ok(!text.includes('=>'), `${marker} by ${by}: a callable, got ${text}`);
        }
      }
    });
  });
});
