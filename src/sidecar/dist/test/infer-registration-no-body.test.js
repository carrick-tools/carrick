/**
 * carrick#827, the producer side: a route whose handler reads nothing from
 * the incoming message published a request type.
 *
 * The scanner locates a producer's request body at the whole registration
 * call (`app.post(path, handler)`). Where the handler declares no request
 * contract and holds no typed read, the inferrer answered nothing. That
 * answer is not a decision, so the capture re-read the same span and
 * published the registration call's own type: `void` wherever the server
 * library's types resolve. The route then states a request type, and every
 * call to it is checked against it.
 *
 * Where the handler reads nothing from its parameters at all, the inferrer
 * now decides: this route takes no request body. Each use of a parameter is a
 * call made on it whose value is discarded or returned (`reply.json(result)`,
 * `return c.json(result)`), which is a send.
 *
 * A handler that reads anything from a parameter keeps the undecided answer
 * it had: a property read, a zero-argument call it uses the value of, or the
 * parameter handed whole to another function may each be the body read, and
 * this cannot tell. So may a call on a parameter that is handed something
 * able to receive (a callback, or an object with a method: `request.pipe(dest)`,
 * `request.on('data', cb)`, `request.json().then(cb)`). A handler with no
 * parameter proves nothing either. No method or property name is consulted.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const ROUTES_TS = `interface Result { y: number }
interface Incoming {
  json(): Promise<any>;
  body: any;
  params: { id: string };
  pipe<T extends { write(chunk: unknown): void }>(destination: T): T;
  on(event: string, listener: (chunk: unknown) => void): Incoming;
}
interface Outgoing { json(value: unknown): void; status(code: number): Outgoing; send(value?: unknown): void }
declare const app: {
  post(path: string, handler: (request: Incoming, reply: Outgoing) => unknown): void;
};
declare const parser: { write(chunk: unknown): void; end(): void };
declare function audit(request: Incoming): void;
declare function store(body: unknown): void;
declare const result: Result;

app.post('/sends-only', (request, reply) => {
  reply.json(result);
});

app.post('/returns-a-send', (request, reply) => reply.status(201).send(result));

app.post('/no-parameters', () => {
  return result;
});

app.post('/sends-a-primitive', (request, reply) => {
  reply.status(201).send('ok');
});

app.post('/pipes-the-request', (request, reply) => {
  request.pipe(parser);
  reply.send();
});

app.post('/listens-on-the-request', (request, reply) => {
  const chunks: unknown[] = [];
  request.on('data', (chunk) => {
    chunks.push(chunk);
  });
  reply.send();
});

app.post('/reads-through-a-callback', (request, reply) => {
  request.json().then(store);
  reply.send();
});

app.post('/reads-through-this', function (this: { request: Incoming }) {
  return this.request.body;
});

app.post('/reads-through-this-beside-parameters', function (
  this: { request: { body: Result } },
  request,
  reply
) {
  reply.json(this.request.body);
});

app.post('/reads-a-property', (request, reply) => {
  const name = request.body.name;
  reply.json({ name });
});

app.post('/reads-by-a-call', async (request, reply) => {
  const body = await request.json();
  reply.json(body);
});

app.post('/echoes-a-read', (request) => request.json());

app.post('/hands-the-request-on', (request, reply) => {
  audit(request);
  reply.json(result);
});

app.post('/reads-a-path-parameter', (request, reply) => {
  reply.json({ id: request.params.id });
});

app.post('/typed-read', async (request, reply) => {
  const body = (await request.json()) as { name: string };
  reply.json(body);
});
`;
/** The span of the registration call for `route`, as the scanner sends it. */
function registration(route) {
    const start = ROUTES_TS.indexOf(`app.post('${route}'`);
    assert.ok(start >= 0, `fixture must register ${route}`);
    let depth = 0;
    for (let i = start; i < ROUTES_TS.length; i += 1) {
        if (ROUTES_TS[i] === '(')
            depth += 1;
        if (ROUTES_TS[i] === ')') {
            depth -= 1;
            if (depth === 0) {
                return { start, end: i + 1, line: ROUTES_TS.slice(0, start).split('\n').length };
            }
        }
    }
    throw new Error(`unbalanced registration for ${route}`);
}
describe('carrick#827: a registration whose handler reads nothing takes no request body', () => {
    let client;
    let repoDir;
    let routesPath;
    before(async () => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-827-'));
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
    /** The request-body answer for a route located at its registration call. */
    const requestBody = async (route) => {
        const span = registration(route);
        const alias = `Request_${route.replace(/\W/g, '_')}`;
        const response = await client.send({
            action: 'infer',
            request_id: `infer-${alias}`,
            requests: [
                {
                    file_path: routesPath,
                    line_number: span.line,
                    span_start: span.start,
                    span_end: span.end,
                    infer_kind: 'request_body',
                    alias,
                },
            ],
        });
        return response.inferred_types?.find((t) => t.alias === alias);
    };
    const decidedNoBody = async (route) => {
        const inferred = await requestBody(route);
        assert.ok(inferred, `${route}: the decision must ride the row, or the capture re-reads the call`);
        assert.strictEqual(inferred.type_string.trim(), 'unknown');
        assert.deepStrictEqual((inferred.any_provenance ?? []).map((p) => [p.path, p.reason]), [['', 'no_request_body']]);
    };
    it('decides no body where the handler only sends', async () => {
        await decidedNoBody('/sends-only');
    });
    it('decides no body where the handler returns a send', async () => {
        await decidedNoBody('/returns-a-send');
    });
    it('decides no body where the handler sends a primitive', async () => {
        await decidedNoBody('/sends-a-primitive');
    });
    // A handler that reads anything from a parameter is left undecided, as it
    // was: the read may be the body.
    for (const route of [
        '/reads-a-property',
        '/reads-by-a-call',
        '/echoes-a-read',
        '/hands-the-request-on',
        '/reads-a-path-parameter',
        // A call on a parameter that hands it something able to receive (a
        // callback, an object with methods) may move the body into it.
        '/pipes-the-request',
        '/listens-on-the-request',
        '/reads-through-a-callback',
        // With no parameter there is nothing to prove the handler reads nothing:
        // it may read the message from elsewhere (`this`, a closure).
        '/no-parameters',
        '/reads-through-this',
        '/reads-through-this-beside-parameters',
    ]) {
        it(`stays undecided where the handler reads from a parameter (${route})`, async () => {
            assert.strictEqual(await requestBody(route), undefined);
        });
    }
    it('still publishes the type a read states', async () => {
        const inferred = await requestBody('/typed-read');
        assert.strictEqual(inferred?.type_string.replace(/\s+/g, ' ').trim(), '{ name: string; }');
    });
});
