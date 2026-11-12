/**
 * carrick#2427: a response located at an error-status send is not the route's
 * response. A handler that sends through a parameter (`res.status(409).json(e)`
 * as a statement, nothing returned) has its error sends come first, and the
 * model's locator can name one. The contract is the success body: a 2xx send
 * or one that states no status. Only error sends, plus a 204 or an empty
 * success, is no body at all, never the error body.
 *
 * The status is read from the code (a literal in the send's own chain or
 * argument); a status the source does not fix keeps the located send, as it
 * did before. `res-kit` is a synthetic stand-in: no framework name is read.
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
export interface Req { body: unknown }
export declare const reply: Res;
export declare class App {
  post(path: string, handler: (req: Req, res: Res) => void | Promise<void>): App;
  get(path: string, handler: (req: Req, res: Res) => void | Promise<void>): App;
}
`;

const ROUTES_TS = `import { App } from "res-kit";

interface Item { id: string; label: string }

declare function save(body: unknown): Promise<{ ok: boolean; item: Item }>;
declare const code: number;
declare const token: string;
declare const fresh: boolean;
declare function loadUser(): Promise<{ ok: boolean; user: { name: string; email: string } }>;

const app = new App();

app.post('/items', async (req, res) => {
  const result = await save(req.body);
  if (!result.ok) {
    res.status(500).json({ error: { code: 'failed', message: 'save failed' } });
    return;
  }
  res.json({ created: result.item });
});

app.post('/release', async (req, res) => {
  const result = await save(req.body);
  if (!result.ok) {
    res.status(409).json({ error: { code: 'conflict', message: 'held' } });
    return;
  }
  res.status(204).send();
});

app.get('/only-error', async (req, res) => {
  res.status(404).json({ missing: true });
});

app.get('/unknown-status', async (req, res) => {
  res.status(code).json({ problem: 'upstream' });
});

app.get('/late-success', async (req, res) => {
  const result = await save(req.body);
  if (result.ok) {
    res.json({ fine: result.item });
    return;
  }
  res.status(422).json({ invalid: result.item.id });
});

app.get('/two-success', async (req, res) => {
  const result = await save(req.body);
  if (!result.ok) {
    res.status(400).json({ bad: true });
    return;
  }
  if (result.item.label) {
    res.status(201).json({ made: result.item.label });
    return;
  }
  res.json({ none: true });
});

app.get('/setter-then-return', async (req, res) => {
  const loaded = await loadUser();
  res.set({ 'Cache-Control': 'no-store' });
  return res.json(loaded.user);
});

app.post('/cookie-then-return', async (req, res) => {
  const loaded = await loadUser();
  res.cookie('s', token, { httpOnly: true });
  return res.json(loaded.user);
});

app.post('/conflict-then-setter', async (req, res) => {
  const loaded = await loadUser();
  if (!loaded.ok) {
    res.status(409).json({ clash: true });
    return;
  }
  res.set({ 'Cache-Control': 'no-store' });
  res.json(loaded.user);
});

app.get('/guarded-setter', async (req, res) => {
  const loaded = await loadUser();
  if (!loaded.ok) {
    res.status(404).json({ gone: true });
    return;
  }
  if (fresh) {
    res.set({ 'Cache-Control': 'no-store' });
  }
  res.json(loaded.user);
});
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

describe('carrick#2427 a located error send is not the route response', () => {
  let repoDir: string;
  let routesPath: string;
  let client: SidecarClient;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2427-'));
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

  async function infer(alias: string, route: string, expressionText: string) {
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: routesPath,
          line_number: lineOf(`('${route}'`),
          infer_kind: 'response_body',
          alias,
          expression_text: expressionText,
          expression_line: lineOf(expressionText, ROUTES_TS.indexOf(`('${route}'`)),
        },
      ],
      server_claims: replySendClaims('res-kit', repoDir, ['reply'], ['json', 'send']),
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  it('publishes the success body when the locator names the 500 send', async () => {
    const got = await infer('ItemsFromError', '/items', "{ error: { code: 'failed', message: 'save failed' } }");
    assert.ok(got, 'the route has a success body');
    const text = collapse(got.type_string);
    assert.ok(!/error|failed/.test(text), `the error body is not the contract: ${text}`);
    assert.ok(/created/.test(text), `the success body is: ${text}`);
  });

  it('answers the same from the success send', async () => {
    const fromError = await infer('ItemsA', '/items', "{ error: { code: 'failed', message: 'save failed' } }");
    const fromOk = await infer('ItemsB', '/items', '{ created: result.item }');
    assert.ok(fromError && fromOk);
    assert.strictEqual(collapse(fromError.type_string), collapse(fromOk.type_string));
  });

  it('publishes no body for a 409 beside a 204', async () => {
    const got = await infer('ReleaseFromError', '/release', "{ error: { code: 'conflict', message: 'held' } }");
    const text = got ? collapse(got.type_string) : '';
    assert.ok(!/conflict|error/.test(text), `the 409 body is not the contract: ${text}`);
    assert.ok(
      !got || (got.any_provenance ?? []).some((p) => p.reason === 'no_success_payload'),
      `a route with no success body states that: ${JSON.stringify(got)}`
    );
  });

  it('publishes no body for a route that only sends an error', async () => {
    const got = await infer('OnlyError', '/only-error', '{ missing: true }');
    const text = got ? collapse(got.type_string) : '';
    assert.ok(!/missing/.test(text), `the 404 body is not the contract: ${text}`);
  });

  it('keeps the located send when its status is not fixed by the code', async () => {
    const got = await infer('Unknown', '/unknown-status', "{ problem: 'upstream' }");
    assert.ok(got, 'an unreadable status keeps today\'s answer');
    assert.ok(/problem/.test(collapse(got.type_string)), collapse(got.type_string));
  });

  it('publishes the success body when the error send comes last', async () => {
    const got = await infer('Late', '/late-success', '{ invalid: result.item.id }');
    assert.ok(got);
    const text = collapse(got.type_string);
    assert.ok(/fine/.test(text) && !/invalid/.test(text), text);
  });

  it('joins every success send and drops the error send', async () => {
    const got = await infer('Two', '/two-success', '{ bad: true }');
    assert.ok(got);
    const text = collapse(got.type_string);
    assert.ok(/made/.test(text) && /none/.test(text) && !/bad/.test(text), text);
  });

  // A setter that returns the response (headers, cookies) is a call whose
  // result is response machinery, but it sends no body: its object argument
  // is never a success body, whichever send the locator names.
  const userOnly = (text: string): boolean =>
    /name/.test(text) && /email/.test(text) && !/Cache-Control|httpOnly|clash|gone/.test(text);

  it('a setter before a returned send is not a body (located at the send)', async () => {
    const got = await infer('SetterReturn', '/setter-then-return', 'loaded.user');
    assert.ok(got);
    const text = collapse(got.type_string);
    assert.ok(userOnly(text), text);
  });

  it('a cookie setter before a returned send is not a body (located at the send)', async () => {
    const got = await infer('CookieReturn', '/cookie-then-return', 'loaded.user');
    assert.ok(got);
    const text = collapse(got.type_string);
    assert.ok(userOnly(text), text);
  });

  it('a setter statement after a located error is not a success body', async () => {
    const got = await infer('ConflictSetter', '/conflict-then-setter', '{ clash: true }');
    assert.ok(got);
    const text = collapse(got.type_string);
    assert.ok(userOnly(text), text);
  });

  it('a guarded setter does not end the handler, so it is not a success body', async () => {
    const got = await infer('GuardedSetter', '/guarded-setter', '{ gone: true }');
    assert.ok(got);
    const text = collapse(got.type_string);
    assert.ok(userOnly(text), text);
  });
});
