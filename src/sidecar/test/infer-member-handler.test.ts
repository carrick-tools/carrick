/**
 * carrick#1969 T2: a registration handed a handler named as a member
 * (`router.get('/x', ctl.list)`) is followed to the function the member
 * names, the way a handler named by a plain identifier already is. A member
 * bound to a wrapper call goes through the wrapper's reply verdict, so a
 * wrapper that builds the reply leaves the route unread.
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
  body: unknown;
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

export type AuthedRequest = Request & { user: { id: string } };
export type GuardedHandler = (req: AuthedRequest, res: Reply) => Promise<unknown> | unknown;

// Sends what the callback returns, unchanged.
export function serve<T>(fn: (req: Request) => Promise<T>): Handler {
  return async (req, res) => {
    res.json(await fn(req));
  };
}

export const kit = { serve };

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

const CONTROLLERS = `import { reply, type Reply, type Request } from 'reply-kit';
import { listWidgets } from './store';
import { guarded, guardedEnvelope, kit, serve } from './wrap';

export const ctl = {
  list: guarded(async (req) => listWidgets()),
  create: guarded(async (req) => {
    const body = req.body as { name: string };
    return { id: 'a', name: body.name };
  }),
  enveloped: guardedEnvelope(async (req) => listWidgets()),
  plain: async (req: Request) => listWidgets(),
  method(req: Request): Promise<{ id: string }[]> {
    return Promise.resolve([{ id: 'a' }]);
  },
};

const later = {
  list: guarded(async (req) => listWidgets()),
};
export default later;

const named = async (req: Request) => listWidgets();
const shorthand = { named };
export { shorthand };

export class Controller {
  list = async (req: Request) => listWidgets();
  wrapped = serve(async (req) => listWidgets());
  show(req: Request): Promise<{ id: string }[]> {
    return Promise.resolve([{ id: 'a' }]);
  }
}
export const instance = new Controller();

export async function listAll(req: Request) {
  return listWidgets();
}
export const wrappedAll = serve(async (req) => listWidgets());
export const viaKit = kit.serve(async (req) => listWidgets());

export function requireKey(req: Request, res: Reply, next: () => void): void {
  if (!req.user) {
    res.status(401).json({ message: 'no' });
    return;
  }
  next();
}
`;

const ROUTES = `import make from 'reply-kit';
import later, { ctl, shorthand, instance, requireKey } from './controllers';
import * as ns from './controllers';

const app = make();

app.get('/list', ctl.list);
app.post('/create', ctl.create);
app.get('/later', later.list);
app.post('/guarded-create', requireKey, ctl.create);
app.get('/enveloped', ctl.enveloped);
app.get('/plain', ctl.plain);
app.get('/method', ctl.method);
app.get('/shorthand', shorthand.named);
app.get('/field', instance.list);
app.get('/field-wrapped', instance.wrapped);
app.get('/class-method', instance.show);
app.get('/ns-fn', ns.listAll);
app.get('/ns-wrapped', ns.wrappedAll);
app.get('/kit-serve', ns.viaKit);
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
  check('reply', 'export', { kind: 'op', op: 'send', member: 'body', on: 'export', form: 'set', payload: { arg: 0 } }),
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


describe('carrick#1969 T2: a handler named as a member is followed to its function', () => {
  let root: string;
  let routesPath: string;
  let source: string;
  let client: SidecarClient;
  let id = 0;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-member-handler-'));
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
    fs.writeFileSync(path.join(root, 'src', 'controllers.ts'), CONTROLLERS);
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

  /** The row for the registration of `route`, asked as the scan asks it: a span over the call. */
  const infer = async (route: string, kind: Kind): Promise<Inferred | undefined> => {
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
            span_start: start,
            span_end: end,
          },
        ],
        server_claims: { from_dir: root, checks: LABELS, type_packages: {} },
      },
      60_000
    );
    return response.inferred_types?.find((t) => t.alias === alias);
  };
  const text = async (route: string, kind: Kind) => collapse((await infer(route, kind))?.type_string);

  describe('a member bound to a wrapper that sends what the callback returns', () => {
    it('the response is what the callback returns', async () => {
      assert.strictEqual(await text('/list', 'response_body'), WIDGETS);
    });

    it('the request is read off the callback', async () => {
      assert.strictEqual(await text('/create', 'request_body'), '{ name: string; }');
      assert.strictEqual(await text('/create', 'response_body'), '{ id: string; name: string; }');
    });

    it('an object declared first and exported by a later default statement', async () => {
      assert.strictEqual(await text('/later', 'response_body'), WIDGETS);
    });

    it('a middleware before the member is not the handler', async () => {
      assert.strictEqual(await text('/guarded-create', 'request_body'), '{ name: string; }');
      assert.strictEqual(await text('/guarded-create', 'response_body'), '{ id: string; name: string; }');
    });
  });

  it('a member bound to a wrapper that builds the reply is unread, never the callback`s return', async () => {
    assert.strictEqual(await infer('/enveloped', 'response_body'), undefined);
  });

  describe('every member form names its function', () => {
    const forms: Array<[string, string]> = [
      ['/plain', WIDGETS],
      ['/method', '{ id: string; }[]'],
      ['/shorthand', WIDGETS],
      ['/field', WIDGETS],
      ['/field-wrapped', WIDGETS],
      ['/class-method', '{ id: string; }[]'],
      ['/ns-fn', WIDGETS],
      ['/ns-wrapped', WIDGETS],
    ];
    for (const [route, wanted] of forms) {
      it(`${route}: the response is what the member returns`, async () => {
        assert.strictEqual(await text(route, 'response_body'), wanted);
      });
    }
  });

  it('a wrapper called as a method with a value callback is read by its own code', async () => {
    assert.strictEqual(await text('/kit-serve', 'response_body'), WIDGETS);
  });
});
