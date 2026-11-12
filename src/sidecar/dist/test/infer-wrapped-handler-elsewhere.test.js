/**
 * carrick#2472 P3-2b: a route whose handler is declared in another file, as
 * a name bound to a wrapper call, behind middleware.
 *
 * `router.route('/items').post(auth, audit, createItem)` registers three
 * functions. The handler is the last of them; the others are middleware. The
 * inferrer read the first function argument, so the route's request and
 * response were the middleware's. And `createItem` is not a function
 * literal: it is `wrap<CreateItem>(async (req, res) => …)`, a call whose value
 * is callable and which is handed exactly one function. The inferrer did not
 * follow that call, so the handler resolved to nothing and the capture
 * re-read the registration line and served the router.
 *
 * Now the handler is the last argument that resolves to a function, and a
 * name bound to such a wrapper call resolves to the one function it is
 * handed, in whatever file it is declared. The wrapper's type argument types
 * that function's parameters, so the route's request is `CreateItem['Body']`
 * and a response it returns is read off it.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const HTTP_TS = `export interface Endpoint { Body: unknown; Reply: unknown }
export interface Req<B> { body: B; params: Record<string, string> }
export interface Res<R> {
  status(code: number): Res<R>;
  send(body: R): void;
}
export type Handler = (req: Req<any>, res: Res<any>, next: () => void) => unknown;
export declare const router: {
  route(path: string): {
    post(...handlers: Handler[]): void;
    get(...handlers: Handler[]): void;
  };
  get(path: string, ...handlers: Handler[]): void;
  post(path: string, ...handlers: Handler[]): void;
};

/** Types a handler's request and response from an endpoint contract. */
export function wrap<T extends Endpoint>(
  fn: (req: Req<T['Body']>, res: Res<T['Reply']>, next: () => void) => Promise<void> | void
): Handler {
  return (req, res, next) => {
    void fn(req, res, next);
  };
}

/** Types a handler that returns its reply from an endpoint contract. */
export function serve<T extends Endpoint>(fn: (req: Req<T['Body']>) => Promise<T['Reply']>): Handler {
  return (req, res) => {
    void fn(req).then((reply) => res.send(reply));
  };
}
`;
const MIDDLEWARE_TS = `import type { Req, Res } from './http.js';

/** A middleware that reads a body of its own: never the route's. */
export function audit(req: Req<{ actor: string }>, res: Res<{ audited: boolean }>, next: () => void): void {
  if (!req.body.actor) {
    res.status(401).send({ audited: false });
    return;
  }
  next();
}

export function auth(req: Req<unknown>, res: Res<unknown>, next: () => void): void {
  next();
}
`;
const HANDLERS_TS = `import { serve, wrap } from './http.js';

export interface CreateItem {
  Body: { name: string; quantity: number };
  Reply: { id: string };
}

export interface GetItem {
  Body: never;
  Reply: { id: string; name: string };
}

export const createItem = wrap<CreateItem>(async (req, res) => {
  res.status(201).send({ id: req.body.name });
});

export const getItem = serve<GetItem>(async (req) => ({ id: req.params.id, name: 'item' }));
`;
const ROUTES_TS = `import { router } from './http.js';
import { audit, auth } from './middleware.js';
import { createItem, getItem } from './handlers.js';
import type { Req, Res } from './http.js';

router.route('/items').post(audit, auth, createItem);
router.get('/items/:id', auth, getItem);
router.post('/inline', async (req: Req<{ title: string }>, res: Res<{ ok: boolean }>) => {
  res.status(201).send({ ok: true });
});
router.route('/audited').post(audit, async (req: Req<{ note: string }>, res: Res<{ saved: boolean }>) => {
  res.status(201).send({ saved: true });
});
`;
const lineOf = (needle) => {
    const at = ROUTES_TS.indexOf(needle);
    assert.ok(at >= 0, `fixture must hold ${needle}`);
    return ROUTES_TS.slice(0, at).split('\n').length;
};
const flat = (text) => text?.replace(/\s+/g, ' ').trim();
describe('carrick#2472 P3-2b: a handler declared in another file, made by a wrapper, behind middleware', () => {
    let client;
    let repoDir;
    let routesPath;
    before(async () => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2472-p3-2b-'));
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
            compilerOptions: {
                strict: true,
                module: 'esnext',
                moduleResolution: 'bundler',
                target: 'es2022',
                skipLibCheck: true,
            },
            include: ['src'],
        }));
        fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
        fs.writeFileSync(path.join(repoDir, 'src', 'http.ts'), HTTP_TS);
        fs.writeFileSync(path.join(repoDir, 'src', 'middleware.ts'), MIDDLEWARE_TS);
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
    /** The answer for a line-only request at a registration, as the scanner sends a named handler's. */
    const atLine = async (needle, kind) => {
        const alias = `${kind}_${lineOf(needle)}`;
        const response = await client.send({
            action: 'infer',
            request_id: `infer-${alias}`,
            requests: [
                { file_path: routesPath, line_number: lineOf(needle), infer_kind: kind, alias },
            ],
        });
        return flat(response.inferred_types?.find((t) => t.alias === alias)?.type_string);
    };
    it('reads the request off the wrapped handler, not the middleware before it', async () => {
        assert.strictEqual(await atLine("router.route('/items')", 'request_body'), '{ name: string; quantity: number; }');
    });
    it('reads the response a wrapped handler returns', async () => {
        assert.strictEqual(await atLine("router.get('/items/:id'", 'response_body'), '{ id: string; name: string; }');
    });
    // A handler that only sends through its response parameter answers nothing
    // at a line-only request, wrapped or inline: reading those sends is the
    // labelled-send work (carrick#2472 C4), not this hop.
    it('answers a wrapped send-only handler as it answers an inline one', async () => {
        assert.strictEqual(await atLine("router.route('/items')", 'response_body'), undefined);
        assert.strictEqual(await atLine("router.post('/inline'", 'response_body'), undefined);
    });
    it('never reads a middleware as the handler', async () => {
        assert.strictEqual(await atLine("router.route('/audited')", 'request_body'), '{ note: string; }');
    });
    it('keeps the inline handler answer', async () => {
        assert.strictEqual(await atLine("router.post('/inline'", 'request_body'), '{ title: string; }');
    });
});
