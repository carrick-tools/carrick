/**
 * carrick#4300: a send-span ask on a status-only call on the reply read the
 * status code as the body.
 *
 * `app.post('/x', async (q, reply) => reply.code(204))` answered `204` when
 * the scanner's span named `reply.code(204)`. The registration-span and line
 * asks abstain there: the call hands the reply back and states only a status,
 * so it names no body. A located call that hands back the reply of the
 * handler it sits in, and is handed a status-shaped value (read as a chain's
 * status call is read, never by method name) in a slot that does not take
 * any value, abstains the same way.
 *
 * A send of a value keeps its reading: a typed send after a status call, the
 * send branch of a send-or-status conditional, a text send, a send of a
 * number and a send of a value declared `unknown`.
 *
 * The server packages are installed under `node_modules`, so their reply
 * types are declared outside the repository.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SERVER_D_TS = `export interface ServerRequest { body: unknown }
export interface ServerReply {
  send(payload?: unknown): ServerReply;
  code(status: number): ServerReply;
}
export type Handler = (request: ServerRequest, reply: ServerReply) => unknown;
export interface ServerInstance { post(path: string, handler: Handler): ServerInstance }
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
declare const found: boolean;
declare const count: number;

export function serverRoutes(app: ServerInstance) {
  app.post('/code', async (_request, reply) => reply.code(204));
  app.post('/code-send', async (_request, reply) => reply.code(201).send([item]));
  app.post('/cond', async (_request, reply) => (found ? reply.send([item]) : reply.code(204)));
  app.post('/text', async (_request, reply) => reply.send('ok'));
  app.post('/unknown', async (request, reply) => reply.send(request.body));
  app.post('/count', async (_request, reply) => reply.send(count));
  app.post('/ok', async (_request, reply) => reply.send(200));
}

export function expressRoutes(app: App) {
  app.post('/status', async (_req, res) => res.status(204));
  app.post('/total', async (_req, res) => res.json(count));
}
`;

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    any_provenance?: Array<{ path: string; kind: string; reason: string }>;
  }>;
}

/** The line a route registers on. */
function registrationLine(route: string): number {
  const start = ROUTES_TS.indexOf(`'${route}'`);
  assert.ok(start >= 0, `fixture must register ${route}`);
  return ROUTES_TS.slice(0, start).split('\n').length;
}

/** The span of `call` inside the handler `route` registers, as the scanner sends it. */
function sendSpan(route: string, call: string): { start: number; end: number; line: number } {
  const start = ROUTES_TS.indexOf(call, ROUTES_TS.indexOf(`'${route}'`));
  assert.ok(start >= 0, `${route} must make the call ${call}`);
  return { start, end: start + call.length, line: ROUTES_TS.slice(0, start).split('\n').length };
}

describe('carrick#4300: a located status-only call on the reply names no body', () => {
  let client: SidecarClient;
  let repoDir: string;
  let routesPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4300-'));
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
        name: 'fixture-4300',
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

  const ask = async (alias: string, located: Record<string, number>) => {
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [{ file_path: routesPath, ...located, infer_kind: 'response_body', alias }],
    });
    return response.inferred_types?.find((t) => t.alias === alias);
  };

  const bySend = (route: string, call: string) => {
    const span = sendSpan(route, call);
    return ask(`Send_${route.replace(/\W/g, '_')}_${span.start}`, {
      line_number: span.line,
      span_start: span.start,
      span_end: span.end,
    });
  };

  const byLine = (route: string) =>
    ask(`Line_${route.replace(/\W/g, '_')}`, { line_number: registrationLine(route) });

  for (const [route, call] of [
    ['/code', 'reply.code(204)'],
    ['/status', 'res.status(204)'],
  ] as const) {
    it(`${route}: a send-span ask on a status-only call abstains, as the line ask does`, async () => {
      const line = await byLine(route);
      assert.strictEqual(line, undefined, `${route}: the line ask abstains`);
      const send = await bySend(route, call);
      assert.strictEqual(send, undefined, `${route}: the send-span ask names no body, got ${send?.type_string}`);
    });
  }

  it('/cond: a send-span ask on the status branch of a send-or-status return abstains', async () => {
    const send = await bySend('/cond', 'reply.code(204)');
    assert.strictEqual(send, undefined, `the status branch names no body, got ${send?.type_string}`);
  });

  it('/cond: the send branch keeps the value it sends (send and line)', async () => {
    for (const answer of [await bySend('/cond', 'reply.send([item])'), await byLine('/cond')]) {
      assert.ok(answer, '/cond must answer');
      assert.match(answer.type_string, /id: string/);
    }
  });

  it('/code-send: a typed send after a status call keeps the value it sends', async () => {
    const send = await bySend('/code-send', 'reply.code(201).send([item])');
    assert.ok(send, '/code-send must answer');
    assert.match(send.type_string, /id: string/);
  });

  it('/text: a text send keeps its reading', async () => {
    const send = await bySend('/text', "reply.send('ok')");
    assert.ok(send, '/text must answer');
    assert.strictEqual(send.type_string, '"ok"');
  });

  for (const [route, call, type] of [
    ['/count', 'reply.send(count)', 'number'],
    ['/ok', 'reply.send(200)', '200'],
    ['/total', 'res.json(count)', 'number'],
  ] as const) {
    it(`${route}: a send of a number into a slot that takes any value keeps its reading`, async () => {
      const send = await bySend(route, call);
      assert.ok(send, `${route} must answer`);
      assert.strictEqual(send.type_string, type);
    });
  }

  it('/unknown: a send of a value declared unknown keeps its answer', async () => {
    const send = await bySend('/unknown', 'reply.send(request.body)');
    assert.ok(send, '/unknown must answer');
    assert.strictEqual(send.type_string, 'unknown');
  });
});

/**
 * carrick#4341: the same rule for a reply the repository declares, and for
 * a handler reached from the registration line (named, imported, chained).
 * Every ask uses the registration line, as the scanner sends it.
 */

const OWN_TYPES_TS = `export interface OwnRequest { body: unknown }
export interface OwnReply { send(value?: unknown): OwnReply; code(status: number): OwnReply }
export interface OwnServer {
  post(path: string, handler: (request: OwnRequest, reply: OwnReply) => unknown): OwnServer;
}
`;

const OWN_HANDLERS_TS = `import type { OwnRequest, OwnReply } from './types';
export async function importedCode(_request: OwnRequest, reply: OwnReply) {
  return reply.code(204);
}
`;

const OWN_ROUTES_TS = `import type { OwnServer, OwnRequest, OwnReply } from './types';
import { importedCode } from './handlers';

interface Item { id: string }
declare const item: Item;
declare const found: boolean;
declare const status: number;

async function namedSend(_request: OwnRequest, reply: OwnReply) {
  return reply.send([item]);
}
async function namedCode(_request: OwnRequest, reply: OwnReply) {
  return reply.code(204);
}
async function namedBoth(_request: OwnRequest, reply: OwnReply) {
  return found ? reply.send([item]) : reply.code(204);
}

export function routes(own: OwnServer) {
  own.post('/inline', async (_request, reply) => reply.code(204));
  own.post('/variable', async (_request, reply) => reply.code(status));
  own.post('/block', async (_request, reply) => {
    return reply.code(204);
  });
  own.post('/named', namedCode);
  own.post('/imported', importedCode);
  own.post('/both', namedBoth);
  own.post('/named-send', namedSend);
  own.post('/chain-send', namedSend).post('/chain-code', namedCode);
  own.post('/chain-code-first', namedCode).post('/chain-send-last', namedSend);
}
`;


describe('carrick#4341: a status-only return on a repository-declared reply names no body', () => {
  let client: SidecarClient;
  let repoDir: string;
  let routesPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4341-'));
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
    fs.writeFileSync(path.join(repoDir, 'package.json'), JSON.stringify({ name: 'fixture-4341' }));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(path.join(repoDir, 'src', 'types.ts'), OWN_TYPES_TS);
    fs.writeFileSync(path.join(repoDir, 'src', 'handlers.ts'), OWN_HANDLERS_TS);
    routesPath = path.join(repoDir, 'src', 'routes.ts');
    fs.writeFileSync(routesPath, OWN_ROUTES_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const lineOf = (route: string): number => {
    const start = OWN_ROUTES_TS.indexOf(`'${route}'`);
    assert.ok(start >= 0, `fixture must register ${route}`);
    return OWN_ROUTES_TS.slice(0, start).split('\n').length;
  };

  /** The registration call of `route`, from `own.post(` to its closing paren. */
  const registrationSpan = (route: string) => {
    const start = OWN_ROUTES_TS.lastIndexOf('own.post(', OWN_ROUTES_TS.indexOf(`'${route}'`));
    let depth = 0;
    let end = start;
    for (let i = OWN_ROUTES_TS.indexOf('(', start); i < OWN_ROUTES_TS.length; i++) {
      if (OWN_ROUTES_TS[i] === '(') depth++;
      if (OWN_ROUTES_TS[i] === ')' && --depth === 0) {
        end = i + 1;
        break;
      }
    }
    return { span_start: start, span_end: end };
  };

  const ask = async (alias: string, located: Record<string, unknown>) => {
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        { file_path: routesPath, infer_kind: 'response_body', alias, ...located },
      ],
    });
    return response.inferred_types?.find((t) => t.alias === alias);
  };

  const byLine = (route: string) =>
    ask(`Line_${route.replace(/\W/g, '_')}`, { line_number: lineOf(route) });
  const bySpan = (route: string) =>
    ask(`Span_${route.replace(/\W/g, '_')}`, {
      line_number: lineOf(route),
      ...registrationSpan(route),
    });
  const byText = (route: string, text: string) =>
    ask(`Text_${route.replace(/\W/g, '_')}`, {
      line_number: lineOf(route),
      expression_text: text,
      expression_line: lineOf(route),
    });

  for (const route of ['inline', 'variable', 'block', 'named', 'imported']) {
    it(`/${route}: the line and registration-span asks abstain`, async () => {
      for (const answer of [await byLine(`/${route}`), await bySpan(`/${route}`)]) {
        assert.strictEqual(answer, undefined, `/${route} states only a status, got ${answer?.type_string}`);
      }
    });
  }

  for (const [route, text] of [
    ['/inline', 'reply.code(204)'],
    ['/variable', 'reply.code(status)'],
    ['/block', 'reply.code(204)'],
    ['/named', 'reply.code(204)'],
  ] as const) {
    it(`/${route.slice(1)}: a text-located ask on the status call abstains`, async () => {
      const answer = await byText(route, text);
      assert.strictEqual(answer, undefined, `${route}: got ${JSON.stringify(answer)}`);
    });
  }

  it('/both: the status branch asked by text abstains', async () => {
    const answer = await byText('/both', 'reply.code(204)');
    assert.strictEqual(answer, undefined, `got ${answer?.type_string}`);
  });

  it('/both: the line and registration-span asks keep the send branch', async () => {
    for (const answer of [await byLine('/both'), await bySpan('/both')]) {
      assert.ok(answer, '/both must answer');
      assert.match(answer.type_string, /id: string/);
    }
  });

  it('/named-send: a text-located send keeps its answer', async () => {
    const answer = await byText('/named-send', 'reply.send([item])');
    assert.ok(answer, '/named-send must answer');
    assert.match(answer.type_string, /id: string/);
  });
});

describe('carrick#4309: a line that registers two routes reads each handler as registered', () => {
  let client: SidecarClient;
  let repoDir: string;
  let routesPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4309-'));
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
    fs.writeFileSync(path.join(repoDir, 'package.json'), JSON.stringify({ name: 'fixture-4309' }));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(path.join(repoDir, 'src', 'types.ts'), OWN_TYPES_TS);
    fs.writeFileSync(path.join(repoDir, 'src', 'handlers.ts'), OWN_HANDLERS_TS);
    routesPath = path.join(repoDir, 'src', 'routes.ts');
    fs.writeFileSync(routesPath, OWN_ROUTES_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const spanOf = (route: string) => {
    const at = OWN_ROUTES_TS.indexOf(`.post('${route}'`) + 1;
    const open = OWN_ROUTES_TS.indexOf('(', at);
    let depth = 0;
    let end = open;
    for (let i = open; i < OWN_ROUTES_TS.length; i++) {
      if (OWN_ROUTES_TS[i] === '(') depth++;
      if (OWN_ROUTES_TS[i] === ')' && --depth === 0) {
        end = i + 1;
        break;
      }
    }
    return {
      line_number: OWN_ROUTES_TS.slice(0, at).split('\n').length,
      span_start: at,
      span_end: end,
    };
  };

  const ask = async (route: string) => {
    const alias = `Chain_${route.replace(/\W/g, '_')}`;
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        { file_path: routesPath, infer_kind: 'response_body', alias, ...spanOf(route) },
      ],
    });
    return response.inferred_types?.find((t) => t.alias === alias);
  };

  for (const route of ['/chain-send', '/chain-send-last']) {
    it(`${route}: the send reads its array, not the reply object`, async () => {
      const answer = await ask(route);
      assert.ok(answer, `${route} must answer`);
      assert.match(answer.type_string, /id: string/);
    });
  }

  for (const route of ['/chain-code', '/chain-code-first']) {
    it(`${route}: the status-only route answers nothing`, async () => {
      const answer = await ask(route);
      assert.strictEqual(answer, undefined, `${route}: got ${JSON.stringify(answer)}`);
    });
  }
});
