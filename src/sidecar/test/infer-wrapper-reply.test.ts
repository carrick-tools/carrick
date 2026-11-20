/**
 * carrick#1969 T1: a route handler bound to a wrapper call is read through the
 * wrapper only where the wrapper's own code sends what the handler sends.
 *
 * `const list = wrap(async (req, res) => …)` registered as `app.get('/x',
 * list)` was read as the callback, whatever the wrapper does with the
 * callback's result. A wrapper that builds the reply (`res.json({ data: await
 * fn(req), ok: true })`) then published the callback's bare return, which is
 * not what goes on the wire. The wrapper is now classified by its code:
 *
 * - it hands the callback back, drops or returns its result, or sends it
 *   unchanged as the whole body of a send on a parameter of the function it
 *   returns: the callback is the handler, as before;
 * - it hands the callback a thunk to a helper that sends the thunk's result
 *   unchanged (one hop): the callback is the handler;
 * - it does anything else with the result and returns one function: that
 *   function's reply is the route's;
 * - a wrapper with no body in the program is judged by its declared callback
 *   type: a callback declared to return nothing is the handler, any other
 *   callback leaves the route unread.
 *
 * The server library is invented, with send labels as the store answers them.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const PKG = 'reply-kit';

const KIT = `export interface Reply<B = unknown> {
  statusCode: number;
  headersSent: boolean;
  status(code: number): Reply<B>;
  json(body: B): Reply<B>;
  send(body?: B): Reply<B>;
  header(name: string, value: string): Reply<B>;
  trace(value: unknown): Reply<B>;
}
export interface Request {
  body: unknown;
  user?: { id: string };
}
export type Handler = (req: Request, res: Reply, next: () => void) => void | Promise<void>;
export interface App {
  get(path: string, ...handlers: Handler[]): App;
  post(path: string, ...handlers: Handler[]): App;
}
declare function make(): App;
export default make;
export declare const reply: Reply;

/** A library wrapper whose callback returns nothing (no body in the program). */
export declare function catchErrors(fn: (req: Request, res: Reply) => Promise<void> | void): Handler;
/** A library wrapper whose callback returns the body (no body in the program). */
export declare function respondWith<T>(fn: (req: Request) => Promise<T>): Handler;
`;

const STORE = `export interface Widget { id: string; name: string }
export async function listWidgets(): Promise<Widget[]> {
  return [];
}
`;

const WRAP = `import type { Handler, Reply, Request } from 'reply-kit';
import type { Widget } from './store';

type Next = () => void;
export type AuthedRequest = Request & { user: { id: string } };
export type GuardedHandler = (req: AuthedRequest, res: Reply) => Promise<unknown> | unknown;

// Drops the callback's result.
export function passThrough(fn: (req: Request, res: Reply) => Promise<void> | void): Handler {
  return (req, res, next) => {
    Promise.resolve(fn(req, res)).catch(next);
  };
}

// Returns the callback's result through a cast and a catch.
export function castCatch(fn: (req: Request, res: Reply, next: Next) => Promise<void> | void): Handler {
  return (req, res, next) => {
    return (fn(req, res, next) as unknown as Promise<any>).catch((error: unknown) => next());
  };
}

// Hands the callback to another wrapper inside a function of its own.
export function nested(fn: (req: Request, res: Reply) => Promise<void>): Handler {
  return passThrough(async (req, res) => {
    await fn(req, res);
  });
}

// Hands the callback back.
export function identity<F extends Handler>(fn: F): F {
  return fn;
}

// Sends what the callback returns, unchanged.
export function serve<T>(fn: (req: Request) => Promise<T>): Handler {
  return async (req, res) => {
    res.json(await fn(req));
  };
}

// Sends the callback's result inside another object.
export function envelope<T>(fn: (req: Request) => Promise<T>): Handler {
  return async (req, res) => {
    res.json({ data: await fn(req), ok: true });
  };
}

// The same, with no type parameter.
export function listEnvelope(fn: (req: Request) => Promise<Widget[]>): Handler {
  return async (req, res) => {
    res.json({ items: await fn(req), count: 0 });
  };
}

// Builds the reply from what the callback returns; the callback is handed a
// reply that states a body of its own.
export function statedEnvelope<T>(fn: (req: Request, res: Reply<T>) => Promise<T | undefined>): Handler {
  return async (req, res) => {
    const value = await fn(req, res as Reply<T>);
    if (value !== undefined) replyWith(res, { data: value, ok: true });
  };
}

function replyWith(res: Reply, body: unknown): void {
  res.json(body);
}

export const kit = { serve };

// Hands the callback's result, unchanged, to a member no label names a send.
export function traced<T>(fn: (req: Request) => Promise<T>): Handler {
  return async (req, res) => {
    res.trace(await fn(req));
  };
}

// Checks a signed-in user, then hands a thunk of the callback to a helper
// that sends the thunk's result unchanged.
export function guarded(fn: GuardedHandler): Handler {
  return async (req, res) => {
    if (!req.user) {
      res.status(401).json({ message: 'Unauthorized' });
      return;
    }
    await reply(() => fn(req as AuthedRequest, res), req, res);
  };
}

async function reply(exec: () => Promise<unknown> | unknown, req: Request, res: Reply): Promise<void> {
  try {
    const result = await exec();
    if (!res.headersSent && result !== undefined) res.json(result);
  } catch (error) {
    res.status(500).json({ message: 'Something went wrong' });
  }
}

// The same check, through a helper that sends the thunk's result inside another object.
export function guardedEnvelope(fn: GuardedHandler): Handler {
  return async (req, res) => {
    if (!req.user) {
      res.status(401).json({ message: 'Unauthorized' });
      return;
    }
    await replyEnvelope(() => fn(req as AuthedRequest, res), req, res);
  };
}

async function replyEnvelope(exec: () => Promise<unknown> | unknown, req: Request, res: Reply): Promise<void> {
  const result = await exec();
  res.json({ data: result });
}
`;

const ROUTES = `import make, { catchErrors, respondWith, type Reply, type Request } from 'reply-kit';
import { listWidgets, type Widget } from './store';
import {
  castCatch,
  envelope,
  guarded,
  guardedEnvelope,
  identity,
  kit,
  listEnvelope,
  nested,
  passThrough,
  serve,
  statedEnvelope,
  traced,
} from './wrap';

const passThroughList = passThrough(async (req, res) => {
  res.json(await listWidgets());
});
const castCatchList = castCatch(async (req, res, next) => {
  res.json(await listWidgets());
});
const nestedList = nested(async (req, res) => {
  res.json(await listWidgets());
});
const identityList = identity(async (req, res) => {
  res.json(await listWidgets());
});
const servedList = serve(async (req) => listWidgets());
const envelopedList = envelope(async (req) => listWidgets());
const plainEnvelopedList = listEnvelope(async (req) => listWidgets());
const statedList = statedEnvelope(async (req, res: Reply<Widget[]>) => {
  if (!req.user) {
    res.json([]);
    return undefined;
  }
  return listWidgets();
});
const composedList = passThrough(envelope(async (req) => listWidgets()));
const methodServedList = kit.serve(async (req) => listWidgets());
async function listAll(req: Request) {
  return listWidgets();
}
const envelopedAll = envelope(listAll);
const tracedList = traced(async (req) => listWidgets());
const libVoidList = catchErrors(async (req, res) => {
  res.json(await listWidgets());
});
const libValueList = respondWith(async (req) => listWidgets());
const guardedReturnList = guarded(async (req) => listWidgets());
const guardedSendList = guarded(async (req, res) => {
  res.json(await listWidgets());
});
const guardedEnvelopeList = guardedEnvelope(async (req) => listWidgets());

const app = make();

app.get('/pass-through', passThroughList);
app.get('/cast-catch', castCatchList);
app.get('/nested', nestedList);
app.get('/identity', identityList);
app.get('/served', servedList);
app.post('/enveloped', envelopedList);
app.get('/plain-enveloped', plainEnvelopedList);
app.get('/stated', statedList);
app.get('/lib-void', libVoidList);
app.get('/lib-value', libValueList);
app.get('/guarded-return', guardedReturnList);
app.get('/guarded-send', guardedSendList);
app.get('/guarded-envelope', guardedEnvelopeList);
app.get('/composed', composedList);
app.get('/method-served', methodServedList);
app.get('/all-enveloped', envelopedAll);
app.get('/all', listAll);
app.get('/traced', tracedList);
`;

const TSCONFIG = JSON.stringify({
  compilerOptions: {
    target: 'es2022',
    module: 'commonjs',
    moduleResolution: 'node',
    strict: true,
    skipLibCheck: true,
    types: [],
  },
  include: ['src/**/*.ts'],
});

let sequence = 0;
const check = (exported: string, receiver: string, claim: Record<string, unknown>) => ({
  claim_id: `c${sequence++}`,
  package: PKG,
  export: exported,
  role: 'server_framework',
  receiver,
  claim,
});
const send = (member: string) =>
  check('reply', 'export', { kind: 'op', op: 'send', member, on: 'export', payload: { arg: 0 } });

const LABELS = [
  check('default', 'export', { kind: 'make', form: 'call', member: null }),
  ...['get', 'post'].map((member) =>
    check('default', 'instance:()', {
      kind: 'op',
      op: 'receive',
      member,
      of: 'instance:()',
      name: { arg: 0 },
      handler: { arg: 1 },
      method: member.toUpperCase(),
      request: { param: 0, body: ['body'] },
    })
  ),
  send('json'),
  send('send'),
];

interface Inferred {
  alias: string;
  type_string: string;
  any_provenance?: Array<{ path: string; reason: string }>;
}

type Kind = 'response_body' | 'request_body' | 'function_return';

const collapse = (text: string | undefined): string | undefined =>
  text === undefined ? undefined : text.replace(/\s+/g, ' ').trim();
const WIDGETS = '{ id: string; name: string; }[]';

describe('carrick#1969 T1: a wrapper is read through only where its code sends what the handler sends', () => {
  let root: string;
  let routesPath: string;
  let source: string;
  let client: SidecarClient;
  let id = 0;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-wrapper-reply-'));
    fs.mkdirSync(path.join(root, 'src'), { recursive: true });
    fs.mkdirSync(path.join(root, 'node_modules', PKG), { recursive: true });
    fs.writeFileSync(path.join(root, 'node_modules', PKG, 'index.d.ts'), KIT);
    fs.writeFileSync(
      path.join(root, 'node_modules', PKG, 'package.json'),
      JSON.stringify({ name: PKG, version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(root, 'tsconfig.json'), TSCONFIG);
    fs.writeFileSync(path.join(root, 'src', 'store.ts'), STORE);
    fs.writeFileSync(path.join(root, 'src', 'wrap.ts'), WRAP);
    routesPath = path.join(root, 'src', 'routes.ts');
    fs.writeFileSync(routesPath, ROUTES);
    source = ROUTES;
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: root });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  /**
   * The row for the registration of `route`, asked as the scan asks it: a span
   * over the whole registration call (no semicolon), or the line alone.
   */
  const infer = async (
    route: string,
    kind: Kind,
    shape: 'span' | 'line' = 'span',
    labels = true
  ): Promise<Inferred | undefined> => {
    const start = source.search(new RegExp(`app\\.(get|post)\\('${route}'`));
    assert.ok(start >= 0, `fixture registers ${route}`);
    const end = source.indexOf(');', start) + 1;
    const alias = `a${id}`;
    const response = await client.send<{ inferred_types?: Inferred[] }>(
      {
        action: 'infer',
        request_id: `infer-${id++}`,
        requests: [
          {
            file_path: routesPath,
            line_number: source.slice(0, start).split('\n').length,
            infer_kind: kind,
            alias,
            ...(shape === 'span' ? { span_start: start, span_end: end } : {}),
          },
        ],
        ...(labels ? { server_claims: { from_dir: root, checks: LABELS, type_packages: {} } } : {}),
      },
      60_000
    );
    return response.inferred_types?.find((t) => t.alias === alias);
  };
  const text = async (route: string, kind: Kind, shape: 'span' | 'line' = 'span', labels = true) =>
    collapse((await infer(route, kind, shape, labels))?.type_string);
  const reason = (inferred: Inferred | undefined) =>
    inferred?.any_provenance?.find((p) => p.path === '')?.reason;

  describe('the callback stays the handler', () => {
    for (const route of ['/pass-through', '/cast-catch', '/nested', '/identity', '/served', '/lib-void']) {
      it(`${route}: the response is what the callback sends or returns`, async () => {
        assert.strictEqual(await text(route, 'response_body'), WIDGETS);
      });
    }
  });

  describe('a wrapper that builds the reply states its own reply', () => {
    it('generic envelope: the response is the wrapper`s reply, open until its callers set it', async () => {
      const got = await infer('/enveloped', 'response_body');
      assert.strictEqual(collapse(got?.type_string), 'unknown');
      assert.strictEqual(reason(got), 'bound_by_callers');
    });

    for (const shape of ['span', 'line'] as const) {
      it(`generic envelope: a ${shape} function_return ask reads no callback return`, async () => {
        assert.strictEqual(await infer('/enveloped', 'function_return', shape), undefined);
      });
    }

    it('generic envelope: the request is still read off the callback', async () => {
      const got = await infer('/enveloped', 'request_body');
      assert.strictEqual(collapse(got?.type_string), 'unknown');
      assert.strictEqual(reason(got), 'no_request_body');
    });

    it('non-generic envelope: the response is the wrapper`s real reply', async () => {
      assert.strictEqual(
        await text('/plain-enveloped', 'response_body'),
        '{ items: { id: string; name: string; }[]; count: number; }'
      );
    });

    // The wrapper replies through a helper the response walk does not read,
    // so the stated-reply reading runs: it reads the wrapper's own reply
    // parameter, which states nothing, not the callback's `Reply<Widget[]>`.
    it('a callback`s stated reply is not the route`s when the wrapper builds the reply', async () => {
      const got = await infer('/stated', 'response_body');
      assert.notStrictEqual(collapse(got?.type_string), WIDGETS);
      assert.strictEqual(got, undefined);
    });
  });

  it('composed wrappers: the innermost wrapper that builds the reply decides', async () => {
    const got = await infer('/composed', 'response_body');
    assert.strictEqual(collapse(got?.type_string), 'unknown');
    assert.strictEqual(reason(got), 'bound_by_callers');
  });

  it('a function handed to an envelope and also registered bare is read as itself where bare', async () => {
    const got = await infer('/all-enveloped', 'response_body');
    assert.strictEqual(reason(got), 'bound_by_callers');
    assert.strictEqual(await text('/all', 'response_body'), WIDGETS);
  });

  it('with send labels, a result handed to a member no label names a send is not sent unchanged', async () => {
    assert.strictEqual(await infer('/traced', 'response_body'), undefined);
  });

  describe('a wrapper with no body in reach is judged by its declared callback', () => {
    it('a library callback declared to return a value leaves the route unread', async () => {
      assert.strictEqual(await infer('/lib-value', 'response_body'), undefined);
    });

    // A wrapper called as a method is not followed to its body (carrick#1969
    // T2 follows members), so its declared callback type decides.
    it('a wrapper called as a method with a value callback leaves the route unread', async () => {
      assert.strictEqual(await infer('/method-served', 'response_body'), undefined);
    });
  });

  describe('a thunk of the callback handed to a reply helper (one hop)', () => {
    it('the helper sends the thunk`s result unchanged: the response is what the callback returns', async () => {
      assert.strictEqual(await text('/guarded-return', 'response_body'), WIDGETS);
    });

    it('the helper sends the thunk`s result unchanged: the response is what the callback sends', async () => {
      assert.strictEqual(await text('/guarded-send', 'response_body'), WIDGETS);
    });

    it('the helper wraps the thunk`s result: the callback`s bare return is not the route`s', async () => {
      assert.strictEqual(await infer('/guarded-envelope', 'response_body'), undefined);
    });
  });
});
