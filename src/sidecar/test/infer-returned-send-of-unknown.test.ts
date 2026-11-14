/**
 * carrick#4269: a handler that returns its reply's send of an `unknown` body
 * published the reply object as the route's response.
 *
 * `async (request, reply) => reply.send(request.body)`, with `request.body`
 * typed `unknown` and `send` returning the reply, read as the send call's own
 * value: the reply's type. The same handler sending a typed value reads the
 * value it sends. A value typed as a parameter the registration hands the
 * handler is never the body, so an untyped send answers `unknown`, never the
 * reply.
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

const ROUTES_TS = `import type { ServerInstance } from 'fixture-server';

interface Item { id: string }
declare const item: Item;

export async function itemRoutes(app: ServerInstance) {
  app.get('/typed', async (_request, reply) => reply.send([item]));
  app.post('/unknown', async (request, reply) => reply.send(request.body));
  app.post('/unknown-chained', async (request, reply) => reply.code(201).send(request.body));
  app.post('/unknown-block', async (request, reply) => {
    return reply.send(request.body);
  });
}
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
  const start = ROUTES_TS.search(new RegExp(`app\\.(get|post)\\('${route}'`));
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
      JSON.stringify({ name: 'fixture-4269', dependencies: { 'fixture-server': '1.0.0' } })
    );
    const pkg = path.join(repoDir, 'node_modules', 'fixture-server');
    fs.mkdirSync(pkg, { recursive: true });
    fs.writeFileSync(
      path.join(pkg, 'package.json'),
      JSON.stringify({ name: 'fixture-server', version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(pkg, 'index.d.ts'), SERVER_D_TS);
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
  const responseBody = async (route: string, by: 'span' | 'line') => {
    const span = registration(route);
    const alias = `Response_${by}_${route.replace(/\W/g, '_')}`;
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      framework_packages: ['fixture-server'],
      requests: [
        {
          file_path: routesPath,
          line_number: span.line,
          ...(by === 'span' ? { span_start: span.start, span_end: span.end } : {}),
          infer_kind: 'response_body',
          alias,
        },
      ],
    });
    return response.inferred_types?.find((t) => t.alias === alias);
  };

  for (const by of ['span', 'line'] as const) {
    it(`a typed send reads the value it sends (control, by ${by})`, async () => {
      const answer = await responseBody('/typed', by);
      assert.ok(answer, 'the typed send must answer');
      assert.match(answer.type_string, /id: string/);
    });

    for (const route of ['/unknown', '/unknown-chained', '/unknown-block']) {
      it(`${route}: an unknown body answers unknown, never the reply (by ${by})`, async () => {
        const answer = await responseBody(route, by);
        assert.ok(answer, `${route} must answer a decided unknown`);
        assert.strictEqual(answer.type_string, 'unknown');
        assert.doesNotMatch(answer.type_string, /ServerReply/);
        assert.ok(
          (answer.any_provenance ?? []).some((p) => p.path === '' && p.kind === 'unknown'),
          `${route} must say why it is unknown: ${JSON.stringify(answer.any_provenance)}`
        );
      });
    }
  }
});
