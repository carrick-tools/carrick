/**
 * carrick#4269: a handler that returns its reply's send of an `unknown` body
 * published the reply object as the route's response.
 *
 * `async (request, reply) => reply.send(request.body)`, with `request.body`
 * typed `unknown` and `send` returning the reply, read as the send call's own
 * value: the reply's type. The same handler sending a typed value reads the
 * value it sends. A value typed as a parameter the registration hands the
 * handler is never the body: the send's argument is read instead. A typed
 * argument is the body; an argument the code declares `unknown` (or `any`)
 * answers that type with the `declared` reason, never the reply.
 *
 * Express-shaped `res.json(x)` returns the response the same way. A function
 * returning its own repository-declared parameter type keeps its answer.
 *
 * The server package is installed under `node_modules`, as in the
 * `discovery-framework` fixture, so its reply type is declared outside the
 * repository.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SERVER_D_TS = `export interface ServerRequest {
  body: unknown;
  params: Record<string, string>;
}
export interface ServerReply {
  send(payload?: unknown): ServerReply;
  code(status: number): ServerReply;
}
export type Handler = (request: ServerRequest, reply: ServerReply) => unknown;
export interface ServerInstance {
  get(path: string, handler: Handler): ServerInstance;
  post(path: string, handler: Handler): ServerInstance;
}
`;

const EXPRESS_D_TS = `export interface Request { body: any }
export interface Response { json(body?: any): Response; status(code: number): Response }
export type Handler = (req: Request, res: Response) => unknown;
export interface App { post(path: string, handler: Handler): App }
`;

const ROUTES_TS = `import type { ServerInstance } from 'fixture-server';
import type { App } from 'fixture-express';

interface Item { id: string }
declare const item: Item;

interface Options { name: string }
export function withDefaults(options: Options): Options {
  const next: Options = { ...options };
  return next;
}

export function expressRoutes(app: App) {
  app.post('/ex-typed', async (req, res) => res.status(201).json([item]));
  app.post('/ex-any', async (req, res) => res.json(req.body));
}

export async function itemRoutes(app: ServerInstance) {
  app.get('/typed', async (_request, reply) => reply.send([item]));
  app.post('/unknown', async (request, reply) => reply.send(request.body));
  app.post('/unknown-chained', async (request, reply) => reply.code(201).send(request.body));
  app.post('/unknown-block', async (request, reply) => {
    return reply.send(request.body);
  });
}

interface OwnRequest { body: unknown }
interface OwnReply { send(value?: unknown): OwnReply; code(status: number): OwnReply }
interface OwnServer { post(path: string, handler: (request: OwnRequest, reply: OwnReply) => unknown): OwnServer }

export function ownRoutes(own: OwnServer) {
  own.post('/own-typed', async (_request, reply) => reply.send([item]));
  own.post('/own-unknown', async (request, reply) => reply.code(201).send(request.body));
  own.post('/own-block', async (_request, reply) => {
    return reply.send([item]);
  });
}

interface Builder { name: string; set(value: string): Builder }
export function configure(builder: Builder): Builder {
  return builder.set('x');
}
interface Journal { lines: string[]; write(entry: unknown): Journal }
export function record(journal: Journal): Journal {
  return journal.write('x');
}
declare const builders: Builder[];
export const renamed = builders.map((builder) => builder.set('x'));
`;

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    any_provenance?: Array<{ path: string; kind: string; reason: string }>;
  }>;
}

/** The span of the registration call for `route`, as the scanner sends it. */
function registration(route: string): { start: number; end: number; line: number } {
  const start = ROUTES_TS.search(new RegExp(`(app|own)\\.(get|post)\\('${route}'`));
  assert.ok(start >= 0, `fixture must register ${route}`);
  let depth = 0;
  for (let i = start; i < ROUTES_TS.length; i += 1) {
    if (ROUTES_TS[i] === '(') depth += 1;
    if (ROUTES_TS[i] === ')') {
      depth -= 1;
      if (depth === 0) {
        return { start, end: i + 1, line: ROUTES_TS.slice(0, start).split('\n').length };
      }
    }
  }
  throw new Error(`unbalanced registration for ${route}`);
}

/**
 * The span of the send a route's handler returns, as the scanner sends it
 * when it names the send: from the receiver to the send's closing paren, on
 * the send's own line.
 */
function returnedSend(route: string): { start: number; end: number; line: number } {
  const span = registration(route);
  const arrow = ROUTES_TS.indexOf('=>', span.start);
  const body = ROUTES_TS.slice(arrow + 2, span.end);
  const offset = body.search(/reply\./);
  assert.ok(offset >= 0, `${route} must return a send`);
  const start = arrow + 2 + offset;
  let depth = 0;
  for (let i = start; i < span.end; i += 1) {
    if (ROUTES_TS[i] === '(') depth += 1;
    if (ROUTES_TS[i] === ')') {
      depth -= 1;
      if (depth === 0 && ROUTES_TS[i + 1] !== '.') {
        return { start, end: i + 1, line: ROUTES_TS.slice(0, start).split('\n').length };
      }
    }
  }
  throw new Error(`unbalanced send for ${route}`);
}

describe('carrick#4269: a returned send of an unknown body never publishes the reply', () => {
  let client: SidecarClient;
  let repoDir: string;
  let routesPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4269-'));
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    fs.writeFileSync(
      path.join(repoDir, 'package.json'),
      JSON.stringify({
        name: 'fixture-4269',
        dependencies: { 'fixture-server': '1.0.0', 'fixture-express': '1.0.0' },
      })
    );
    for (const [name, declarations] of [
      ['fixture-server', SERVER_D_TS],
      ['fixture-express', EXPRESS_D_TS],
    ]) {
      const pkg = path.join(repoDir, 'node_modules', name);
      fs.mkdirSync(pkg, { recursive: true });
      fs.writeFileSync(
        path.join(pkg, 'package.json'),
        JSON.stringify({ name, version: '1.0.0', types: 'index.d.ts' })
      );
      fs.writeFileSync(path.join(pkg, 'index.d.ts'), declarations);
    }
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    routesPath = path.join(repoDir, 'src', 'routes.ts');
    fs.writeFileSync(routesPath, ROUTES_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  /** The response answer for a route, asked by its registration span or by line. */
  const responseBody = async (route: string, by: 'span' | 'line' | 'send') => {
    const span = by === 'send' ? returnedSend(route) : registration(route);
    const alias = `Response_${by}_${route.replace(/\W/g, '_')}`;
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: routesPath,
          line_number: span.line,
          ...(by !== 'line' ? { span_start: span.start, span_end: span.end } : {}),
          infer_kind: 'response_body',
          alias,
        },
      ],
    });
    return response.inferred_types?.find((t) => t.alias === alias);
  };

  for (const by of ['span', 'line'] as const) {
    for (const route of ['/typed', '/ex-typed']) {
      it(`${route}: a typed send reads the value it sends, never the reply (by ${by})`, async () => {
        const answer = await responseBody(route, by);
        assert.ok(answer, `${route} must answer`);
        assert.match(answer.type_string, /id: string/);
      });
    }

    for (const route of ['/unknown', '/unknown-chained', '/unknown-block']) {
      it(`${route}: an unknown body answers unknown as declared, never the reply (by ${by})`, async () => {
        const answer = await responseBody(route, by);
        assert.ok(answer, `${route} must answer a decided unknown`);
        assert.strictEqual(answer.type_string, 'unknown');
        assert.deepStrictEqual(
          (answer.any_provenance ?? []).map((p) => [p.path, p.kind, p.reason]),
          [['', 'unknown', 'declared']]
        );
      });
    }

    it(`/ex-any: an any body answers any as declared, never the response (by ${by})`, async () => {
      const answer = await responseBody('/ex-any', by);
      assert.ok(answer, '/ex-any must answer');
      assert.strictEqual(answer.type_string, 'any');
      assert.deepStrictEqual(
        (answer.any_provenance ?? []).map((p) => [p.path, p.kind, p.reason]),
        [['', 'any', 'declared']]
      );
    });
  }

  // carrick#4283: the same reply declared in the repository is transport too,
  // when the shape says so: the handler a registration hands it to returns a
  // call on it that hands the reply back and is given the value it sends.
  for (const by of ['span', 'line', 'send'] as const) {
    for (const route of ['/own-typed', '/own-block']) {
      it(`${route}: a repository-declared reply's send reads the value it sends (by ${by})`, async () => {
        const answer = await responseBody(route, by);
        assert.ok(answer, `${route} must answer`);
        assert.doesNotMatch(answer.type_string, /send/);
        assert.match(answer.type_string, /id: string/);
      });
    }

    it(`/own-unknown: a repository-declared reply's unknown send answers unknown, never the reply (by ${by})`, async () => {
      const answer = await responseBody('/own-unknown', by);
      assert.ok(answer, '/own-unknown must answer');
      assert.strictEqual(answer.type_string, 'unknown');
    });
  }

  it('a function returning a fluent call on its own repository-declared parameter keeps its answer', async () => {
    const line = ROUTES_TS.slice(0, ROUTES_TS.indexOf('export function configure')).split('\n').length;
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: 'infer-configure',
      requests: [
        { file_path: routesPath, line_number: line, infer_kind: 'function_return', alias: 'Configure' },
      ],
    });
    const answer = response.inferred_types?.find((t) => t.alias === 'Configure');
    assert.ok(answer, 'the function return must answer');
    assert.match(answer.type_string, /name: string/);
  });

  it('a function no registration hands in keeps its answer, even when it returns a fluent any-value call', async () => {
    const line = ROUTES_TS.slice(0, ROUTES_TS.indexOf('export function record')).split('\n').length;
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: 'infer-record',
      requests: [
        { file_path: routesPath, line_number: line, infer_kind: 'function_return', alias: 'Record' },
      ],
    });
    const answer = response.inferred_types?.find((t) => t.alias === 'Record');
    assert.ok(answer, 'the function return must answer');
    assert.match(answer.type_string, /lines: string\[\]/);
  });

  it('a callback returning a typed setter on its own parameter keeps its answer (send and line)', async () => {
    const start = ROUTES_TS.indexOf("builder.set('x')", ROUTES_TS.indexOf('export const renamed'));
    const line = ROUTES_TS.slice(0, start).split('\n').length;
    for (const [alias, extra] of [
      ['RenamedSend', { span_start: start, span_end: start + "builder.set('x')".length }],
      ['RenamedLine', {}],
    ] as const) {
      const response = await client.send<InferShape>({
        action: 'infer',
        request_id: `infer-${alias}`,
        requests: [
          { file_path: routesPath, line_number: line, ...extra, infer_kind: 'response_body', alias },
        ],
      });
      const answer = response.inferred_types?.find((t) => t.alias === alias);
      assert.ok(answer, `${alias} must answer`);
      assert.match(answer.type_string, /name: string/);
    }
  });

  it('a function returning its own repository-declared parameter type keeps its answer', async () => {
    const line = ROUTES_TS.slice(0, ROUTES_TS.indexOf('export function withDefaults')).split('\n').length;
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: 'infer-withDefaults',
      requests: [
        { file_path: routesPath, line_number: line, infer_kind: 'function_return', alias: 'Fn' },
      ],
    });
    const answer = response.inferred_types?.find((t) => t.alias === 'Fn');
    assert.ok(answer, 'the function return must answer');
    assert.match(answer.type_string, /name: string/);
  });
});
