/**
 * carrick#1161: a route response is typed from its error or redirect branch
 * when the model's locator points at the first response call.
 *
 * The analyzer reports ONE response expression per row, and on a handler that
 * guards before it succeeds that expression is the guard's body:
 * `return ctx.json({ error: 'q required' }, 400)`. The located expression is
 * then a plain object literal, so the payload walk that drops a >= 400 branch
 * and unions the rest never ran, and the route's contract was published as its
 * 400 body. A redirect's location argument was published as a `string` body the
 * same way.
 *
 * Design: the locator anchors the HANDLER; the sidecar enumerates the handler's
 * response sites, classifies each by the status it states (a literal argument,
 * a literal-typed argument, or the send call's own status type), drops the
 * error and redirect branches, and unions what survives. A status that is not
 * a literal fails open into the union.
 *
 * The same sends are also the carrick#1163 wire rule: a JSON response carries
 * `toJSON()`'s return type, not the object that had one.
 *
 * Every framework name below is a synthetic stand-in; the send calls carry
 * their status as a TYPE, which is the only thing the classification reads.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const FRAMEWORK_DTS = `export type StatusCode =
  | 200 | 201 | 202 | 204 | 301 | 302 | 303 | 307 | 308
  | 400 | 401 | 403 | 404 | 409 | 413 | 422 | 500 | 502 | 503;
export type RedirectCode = 301 | 302 | 303 | 307 | 308;

/** What a send call hands back: the platform response plus its typed parts. */
export interface Sent<T, S extends number, F extends string> {
  readonly _body: T;
  readonly _code: S;
  readonly _format: F;
}

export interface Context {
  req: {
    query(name: string): string | undefined;
  };
  json<T, S extends StatusCode = StatusCode>(body: T, status?: S): Response & Sent<T, S, 'json'>;
  text<S extends StatusCode = StatusCode>(body: string, status?: S): Response & Sent<string, S, 'text'>;
  redirect<S extends RedirectCode = 302>(location: string, status?: S): Response & Sent<undefined, S, 'redirect'>;
}

export type Handler = (c: Context) => Response | Promise<Response>;

export declare class Router {
  get(path: string, handler: Handler): Router;
  post(path: string, handler: Handler): Router;
}
`;

const ROUTES_TS = `import { Router, type Context } from "route-kit";

interface Hit {
  id: string;
  label: string;
}

interface PaymentView {
  id: string;
  createdAt: Date;
  paidAt: Date | null;
}

class Stamp {
  constructor(private readonly at: number) {}
  toJSON(): string {
    return String(this.at);
  }
}

class Unstamped {
  constructor(readonly at: number) {}
}

declare function search(q: string): Promise<Hit[]>;
declare function loadPayment(id: string): PaymentView;
declare function buildLocation(next: string): string;
declare const unconfigured: boolean;
declare const upstreamStatus: number;

const router = new Router();

router.get('/search', async (c) => {
  const q = c.req.query('q');
  if (!q) {
    return c.json({ error: 'q required' }, 400);
  }
  if (q.length < 2) {
    return c.json({ results: [] as Hit[] });
  }
  try {
    const results = await search(q);
    return c.json({ results });
  } catch (err) {
    if (err instanceof Error) {
      return c.json({ error: err.message }, 400);
    }
    return c.json({ results: [] as Hit[], messageKey: 'search.failed' });
  }
});

router.get('/connect', (c) => {
  const next = c.req.query('next');
  if (!next) {
    return c.json({ error: 'next required' }, 400);
  }
  if (unconfigured) {
    return c.json({ error: 'not configured', details: 'set the client id' }, 503);
  }
  return c.redirect(buildLocation(next));
});

router.get('/connect/callback', async (c) => {
  const code = c.req.query('code');
  if (!code) {
    return c.json({ error: 'missing code' }, 400);
  }
  const back = c.req.query('back');
  if (back) {
    return c.redirect(back);
  }
  return c.json({ success: true, code });
});

async function startFlow(c: Context, kind: string) {
  try {
    return c.redirect('/flow/' + kind);
  } catch (err) {
    let message = 'flow failed';
    let code: 400 | 500 = 500;
    if (err instanceof Error) {
      message = err.message;
      code = 400;
    }
    return c.text(message, code);
  }
}

router.get('/flow/start', (c) => startFlow(c, 'start'));

router.get('/verify', (c) => {
  const challenge = c.req.query('challenge');
  if (challenge !== undefined) {
    return c.text(challenge ?? '', 200);
  }
  return c.text('Forbidden', 403);
});

router.post('/relay', async (c) => {
  if (unconfigured) {
    return c.json({ problem: 'upstream failed' }, upstreamStatus);
  }
  return c.json({ relayed: true });
});

router.post('/forward', async (c) => {
  if (unconfigured) {
    return c.json({ error: 'envelope too large' }, 413);
  }
  const headers: Record<string, string> = {};
  return new Response(null, { status: 202, headers });
});

router.get('/payment', (c) => {
  const payment = loadPayment('1');
  return c.json({ payment, stamp: new Stamp(1), unstamped: new Unstamped(2) });
});
`;

function lineOf(text: string, from = 0): number {
  const at = ROUTES_TS.indexOf(text, from);
  assert.ok(at >= 0, `fixture must contain: ${text}`);
  return ROUTES_TS.slice(0, at).split('\n').length;
}

function registrationLine(route: string): number {
  return lineOf(`('${route}'`);
}

interface ProvenanceShape {
  path: string;
  kind: string;
  reason: string;
}

interface InferShape {
  status: string;
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    any_provenance?: ProvenanceShape[];
  }>;
  errors?: string[];
}

function collapse(text: string): string {
  return text.replace(/\s+/g, ' ').trim();
}

describe('carrick#1161 response sites in the anchored handler', () => {
  let repoDir: string;
  let routesPath: string;
  let client: SidecarClient;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1161-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.mkdirSync(path.join(repoDir, 'node_modules', 'route-kit'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'node_modules', 'route-kit', 'package.json'),
      JSON.stringify({ name: 'route-kit', version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(repoDir, 'node_modules', 'route-kit', 'index.d.ts'), FRAMEWORK_DTS);
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

  /** The locator the scanner sends: registration line + the model's expression. */
  async function inferResponse(
    alias: string,
    route: string,
    expressionText: string,
    expressionFrom = 0
  ) {
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: routesPath,
          line_number: registrationLine(route),
          infer_kind: 'response_body',
          alias,
          expression_text: expressionText,
          expression_line: lineOf(expressionText, expressionFrom),
        },
      ],
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  it('publishes the success union when the locator names the 400 body', async () => {
    const inferred = await inferResponse('SearchFromError', '/search', "{ error: 'q required' }");
    assert.ok(inferred, 'the route has success bodies; it must not abstain');
    const text = collapse(inferred.type_string);
    assert.ok(!/error/.test(text), `an error body is not the contract, got: ${text}`);
    assert.ok(/results/.test(text), `the success bodies are the contract, got: ${text}`);
    assert.ok(/messageKey/.test(text), `every success branch joins the union, got: ${text}`);
  });

  it('converges on one union whichever response row the model reported', async () => {
    const fromError = await inferResponse('SearchA', '/search', "{ error: 'q required' }");
    const fromSuccess = await inferResponse('SearchB', '/search', '{ results }');
    const fromLateError = await inferResponse(
      'SearchC',
      '/search',
      '{ error: err.message }'
    );
    const fromFallback = await inferResponse(
      'SearchD',
      '/search',
      "{ results: [] as Hit[], messageKey: 'search.failed' }"
    );
    assert.ok(fromError && fromSuccess && fromLateError && fromFallback);
    const texts = new Set(
      [fromError, fromSuccess, fromLateError, fromFallback].map((t) => collapse(t.type_string))
    );
    assert.strictEqual(texts.size, 1, `one route, one contract; got ${[...texts].join(' || ')}`);
  });

  it('publishes no body for a redirect-only route, from its error row', async () => {
    const inferred = await inferResponse('ConnectFromError', '/connect', "{ error: 'next required' }");
    assert.ok(inferred, 'the abstain is reported, with its reason');
    assert.strictEqual(collapse(inferred.type_string), 'unknown');
    const provenance = inferred.any_provenance ?? [];
    assert.strictEqual(provenance.length, 1, JSON.stringify(provenance));
    assert.strictEqual(provenance[0].reason, 'no_success_payload');
  });

  it('never reads a redirect location as the body', async () => {
    const inferred = await inferResponse('ConnectFromLocation', '/connect', 'buildLocation(next)');
    assert.ok(inferred, 'the abstain is reported, with its reason');
    assert.strictEqual(collapse(inferred.type_string), 'unknown');
  });

  it('drops the redirect and error branches but keeps the success body', async () => {
    const inferred = await inferResponse('CallbackFromError', '/connect/callback', "{ error: 'missing code' }");
    assert.ok(inferred);
    assert.strictEqual(collapse(inferred.type_string), '{ success: true; code: string; }');
  });

  it('reads a status carried by a literal-typed variable', async () => {
    const inferred = await inferResponse('FlowFromMessage', '/flow/start', 'message', ROUTES_TS.indexOf('return c.text(message'));
    assert.ok(inferred, 'the abstain is reported');
    assert.strictEqual(
      collapse(inferred.type_string),
      'unknown',
      'a text body sent with a 400 | 500 status is an error body, not the route contract'
    );
  });

  it('keeps a success text body the locator names', async () => {
    const inferred = await inferResponse('VerifyText', '/verify', "challenge ?? ''");
    assert.ok(inferred);
    assert.strictEqual(collapse(inferred.type_string), 'string');
  });

  it('fails open into the union when a status is not a literal', async () => {
    const inferred = await inferResponse('RelayFromSuccess', '/relay', '{ relayed: true }');
    assert.ok(inferred);
    const text = collapse(inferred.type_string);
    assert.ok(/relayed/.test(text), text);
    assert.ok(/problem/.test(text), `a variable status is kept, not guessed away: ${text}`);
  });

  it('never reads a response init object as the success body', async () => {
    const inferred = await inferResponse('ForwardFromError', '/forward', "{ error: 'envelope too large' }");
    assert.ok(inferred, 'the abstain is reported');
    assert.strictEqual(
      collapse(inferred.type_string),
      'unknown',
      '`{ status: 202, headers }` beside a null body is init, and the route sends no body'
    );
  });

  it('prints the JSON wire type of a value with toJSON (carrick#1163)', async () => {
    const inferred = await inferResponse(
      'PaymentWire',
      '/payment',
      '{ payment, stamp: new Stamp(1), unstamped: new Unstamped(2) }'
    );
    assert.ok(inferred);
    const text = collapse(inferred.type_string);
    assert.ok(/createdAt: string;/.test(text), `Date sends its toJSON string, got: ${text}`);
    assert.ok(/paidAt: null \| string;/.test(text), `got: ${text}`);
    assert.ok(/stamp: string;/.test(text), `any toJSON maps, not only Date, got: ${text}`);
    assert.ok(/unstamped: \{ at: number; \}/.test(text), `no toJSON, no mapping, got: ${text}`);
    assert.ok(!/Date/.test(text), `got: ${text}`);
  });
});
