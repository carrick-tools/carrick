/**
 * carrick#2463: a row whose line is a route-descriptor object declared away
 * from its handler is not a registration and sits outside the handler, so the
 * response-site walk used to skip it and publish the located error body. When
 * the located send states an error or redirect status, the walk now runs over
 * the function that send sits in, provided that function keeps a send once
 * the error and redirect sends are dropped. `res-kit` is a synthetic
 * stand-in: no framework name is read.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient, replySendClaims } from './helpers.js';

const KIT_DTS = `export interface Res {
  statusCode: number;
  setHeader(name: string, value: string): Res;
  getHeader(name: string): string | undefined;
  status(code: number): Res;
  json(body: unknown): Res;
  send(body?: unknown): Res;
  set(fields: Record<string, string>): Res;
  cookie(name: string, value: string, options?: { httpOnly?: boolean; maxAge?: number }): Res;
}
export interface TypedRes<B> {
  statusCode: number;
  setHeader(name: string, value: string): TypedRes<B>;
  getHeader(name: string): string | undefined;
  status(code: number): TypedRes<B>;
  json(body: B): TypedRes<B>;
  send(body?: B): TypedRes<B>;
  set(fields: Record<string, string>): TypedRes<B>;
  cookie(name: string, value: string, options?: { httpOnly?: boolean; maxAge?: number }): TypedRes<B>;
}
export interface Req { body: unknown }
export declare const reply: Res;
export declare const typedReply: TypedRes<unknown>;
export declare class App {
  put(path: string, handler: (req: Req, res: any) => void | Promise<void>): App;
  post(path: string, handler: (req: Req, res: Res) => void | Promise<void>): App;
  get(path: string, handler: (req: Req, res: Res) => void | Promise<void>): App;
}
`;

const ROUTES_TS = `import { Res, TypedRes } from "res-kit";

declare const code: number;
declare function done(): Promise<boolean>;

type ErrorOnly = { error: { code: 'conflict'; message: string } } | { error: { code: 'gone'; message: string } };

async function leasePlain(res: Res): Promise<void> {
  if (!(await done())) {
    res.status(409).json({ error: { code: 'conflict', message: 'held' } });
    return;
  }
  res.status(204).send();
}
export const plainRoute = { method: 'PUT', path: '/plain' };

async function leaseTyped(res: TypedRes<ErrorOnly>): Promise<void> {
  if (!(await done())) {
    res.status(409).json({ error: { code: 'conflict', message: 'held' } });
    return;
  }
  res.status(204).send();
}
export const typedRoute = { method: 'PUT', path: '/typed' };

async function leaseBody(res: Res): Promise<void> {
  if (!(await done())) {
    res.status(409).json({ clash: true });
    return;
  }
  res.json({ leased: true });
}
export const bodyRoute = { method: 'PUT', path: '/body' };

async function leaseVariable(res: Res): Promise<void> {
  res.status(code).json({ problem: 'x' });
}
export const variableRoute = { method: 'PUT', path: '/variable' };

function sendConflict(res: Res, message: string): void {
  res.status(409).json({ helperError: message });
}
async function leaseHelper(res: Res): Promise<void> {
  if (!(await done())) {
    sendConflict(res, 'held');
    return;
  }
  res.json({ fine: true });
}
export const helperRoute = { method: 'PUT', path: '/helper' };
`;

function lineOf(text: string, from = 0): number {
  const at = ROUTES_TS.indexOf(text, from);
  assert.ok(at >= 0, `fixture must contain: ${text}`);
  return ROUTES_TS.slice(0, at).split('\n').length;
}

interface InferShape {
  status: string;
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    any_provenance?: Array<{ path: string; kind: string; reason: string }>;
  }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#2463 a located error send on a row away from its handler', () => {
  let repoDir: string;
  let routesPath: string;
  let client: SidecarClient;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2463-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.mkdirSync(path.join(repoDir, 'node_modules', 'res-kit'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'node_modules', 'res-kit', 'package.json'),
      JSON.stringify({ name: 'res-kit', version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(repoDir, 'node_modules', 'res-kit', 'index.d.ts'), KIT_DTS);
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022', 'dom'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
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

  // The row's line is the descriptor object; the located send is elsewhere.
  async function infer(alias: string, descriptor: string, expressionText: string, after: string) {
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: routesPath,
          line_number: lineOf(`export const ${descriptor} =`),
          infer_kind: 'response_body',
          alias,
          expression_text: expressionText,
          expression_line: lineOf(expressionText, ROUTES_TS.indexOf(after)),
        },
      ],
      server_claims: replySendClaims('res-kit', repoDir, ['reply','typedReply'], ['json', 'send']),
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }
  const noSuccess = (got?: { any_provenance?: Array<{ reason: string }> }): boolean =>
    (got?.any_provenance ?? []).some((p) => p.reason === 'no_success_payload');

  it('1: a 409 beside a 204 answers no_success_payload', async () => {
    const got = await infer('Plain', 'plainRoute', "{ error: { code: 'conflict', message: 'held' } }", 'function leasePlain');
    assert.ok(got);
    assert.ok(!/conflict|error/.test(collapse(got.type_string)), collapse(got.type_string));
    assert.ok(noSuccess(got), JSON.stringify(got));
  });

  it('2: the same with an error-only typed reply', async () => {
    const got = await infer('Typed', 'typedRoute', "{ error: { code: 'conflict', message: 'held' } }", 'function leaseTyped');
    assert.ok(got);
    assert.ok(!/conflict|error/.test(collapse(got.type_string)), collapse(got.type_string));
    assert.ok(noSuccess(got), JSON.stringify(got));
  });

  it('3: a located 409 beside a success body publishes the success body', async () => {
    const got = await infer('BodyFromError', 'bodyRoute', '{ clash: true }', 'function leaseBody');
    assert.ok(got);
    const text = collapse(got.type_string);
    assert.ok(/leased/.test(text) && !/clash|error/.test(text), text);
  });

  it('4: located at the success send, the same body', async () => {
    const got = await infer('BodyFromOk', 'bodyRoute', '{ leased: true }', 'function leaseBody');
    assert.ok(got);
    const text = collapse(got.type_string);
    assert.ok(/leased/.test(text) && !/clash/.test(text), text);
  });

  it('5: a status the code does not fix keeps the located body', async () => {
    const got = await infer('Variable', 'variableRoute', "{ problem: 'x' }", 'function leaseVariable');
    assert.ok(got);
    assert.ok(/problem/.test(collapse(got.type_string)), collapse(got.type_string));
  });

  it('6: an error send in a helper with no other send does not claim no_success_payload', async () => {
    const got = await infer('Helper', 'helperRoute', '{ helperError: message }', 'function sendConflict');
    assert.ok(!noSuccess(got), JSON.stringify(got));
  });
});
