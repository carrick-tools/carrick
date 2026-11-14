/**
 * carrick#4080: a span at a registration reads the handler the registration
 * names, never the call or its arguments. The scanner sends a span at the
 * registration call when the model's locator fails; a registration handed its
 * handler by name, or any callable, is as much a registration as one handed a
 * function literal. Where no handler can be followed the answer is nothing,
 * not the path literal or the server instance.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const HTTP_TS = `export interface Req<B> { body: B }
export interface Res<R> { send(body: R): void }
export type Handler = (req: Req<any>, res: Res<any>) => unknown;
export interface Server {
  get(path: string, ...handlers: Handler[]): Server;
  post(path: string, ...handlers: Handler[]): Server;
}
export declare const router: Server;
export declare const app: Server;
export declare function wrap(app: Server, fn: Handler): Handler;
`;

const HANDLERS_TS = `export function listItems(): { id: string }[] {
  return [{ id: 'a' }];
}
`;

const ROUTES_TS = `import { router, app, wrap } from './http.js';
import type { Req, Res } from './http.js';
import { listItems } from './handlers.js';

function local(): { ok: boolean } {
  return { ok: true };
}
function quiet(req: Req<unknown>, res: Res<unknown>): void {
  res.send('x');
}
const controller = { list: (): number[] => [1] };
const replacer = { fn: (key: string, value: unknown): unknown => value };

router.get('/inline', () => [{ id: 'a' }]);
router.get('/local', local);
router.get('/items', listItems);
router.get('/member', controller.list);
router.get('/void', quiet);
router.post('/wrapped', wrap(app, async (req, res) => res.send(req.body)));

export function post(body: { name: string }): string {
  return JSON.stringify(body, replacer.fn);
}
`;

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string }>;
}

const flat = (text: string | undefined): string | undefined =>
  text?.replace(/\s+/g, ' ').trim();

describe('carrick#4080: a span at a registration reads its handler', () => {
  let client: SidecarClient;
  let repoDir: string;
  let routesPath: string;
  let n = 0;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4080-'));
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
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(path.join(repoDir, 'src', 'http.ts'), HTTP_TS);
    fs.writeFileSync(path.join(repoDir, 'src', 'handlers.ts'), HANDLERS_TS);
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

  const lineOf = (needle: string): number => {
    const at = ROUTES_TS.indexOf(needle);
    assert.ok(at >= 0, `fixture must hold ${needle}`);
    return ROUTES_TS.slice(0, at).split('\n').length;
  };

  /** The call starting at `head`, to its closing paren, as a span. */
  const spanOf = (head: string): { span_start: number; span_end: number } => {
    const start = ROUTES_TS.indexOf(head);
    assert.ok(start >= 0, `fixture must hold ${head}`);
    let depth = 0;
    for (let i = ROUTES_TS.indexOf('(', start); i < ROUTES_TS.length; i++) {
      if (ROUTES_TS[i] === '(') depth++;
      else if (ROUTES_TS[i] === ')' && --depth === 0) {
        return { span_start: start, span_end: i + 1 };
      }
    }
    assert.fail('unbalanced');
  };

  const ask = async (
    head: string,
    kind: 'request_body' | 'response_body',
    how: 'span' | 'line'
  ): Promise<string | undefined> => {
    const alias = `${kind}_${how}_${n++}`;
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: routesPath,
          line_number: lineOf(head),
          infer_kind: kind,
          alias,
          ...(how === 'span' ? spanOf(head) : {}),
        },
      ],
    });
    return flat(response.inferred_types?.find((t) => t.alias === alias)?.type_string);
  };

  it('(a) an inline handler: the span answer is the line answer', async () => {
    const byLine = await ask("router.get('/inline'", 'response_body', 'line');
    assert.match(byLine ?? '', /id: string/);
    assert.strictEqual(await ask("router.get('/inline'", 'response_body', 'span'), byLine);
  });

  it('(b) a same-file named handler: the handler return, not the path', async () => {
    assert.strictEqual(
      await ask("router.get('/local'", 'response_body', 'span'),
      await ask("router.get('/local'", 'response_body', 'line')
    );
    assert.match((await ask("router.get('/local'", 'response_body', 'span')) ?? '', /ok: boolean/);
  });

  it('(c) an imported named handler: the handler return, not the path', async () => {
    const span = await ask("router.get('/items'", 'response_body', 'span');
    assert.match(span ?? '', /id: string/);
    assert.strictEqual(span, await ask("router.get('/items'", 'response_body', 'line'));
  });

  it('(d) a method reference no handler resolves: nothing, not the path', async () => {
    assert.strictEqual(await ask("router.get('/member'", 'response_body', 'span'), undefined);
  });

  it('(e) a handler that returns void: nothing, not the path', async () => {
    assert.strictEqual(await ask("router.get('/void'", 'response_body', 'span'), undefined);
  });

  it('(f) an inline wrapper call: no request and no response from the call itself', async () => {
    assert.strictEqual(await ask("router.post('/wrapped'", 'request_body', 'span'), undefined);
    assert.strictEqual(await ask("router.post('/wrapped'", 'response_body', 'span'), undefined);
  });

  it('(g) a consumer call with a callable argument keeps its answer', async () => {
    assert.strictEqual(
      await ask('JSON.stringify(body, replacer.fn)', 'request_body', 'span'),
      '{ name: string; }'
    );
  });
});
