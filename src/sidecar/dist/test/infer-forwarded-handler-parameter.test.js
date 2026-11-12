/**
 * Regression for carrick-cloud#2576: a route handler that hands its own
 * request and reply objects to a helper (`return finish(request, reply)`) was
 * published with the REQUEST object as its response body.
 *
 * The located response is the helper call. Its result is not data (the helper
 * sends through the reply and hands back what the send returns), so the
 * transitional send drill read the call's first argument as the payload: the
 * handler's request parameter, typed by the framework from the route's type
 * argument (`{ Querystring: ... }`). A compare then reported the request's
 * fields as fields the producer sends.
 *
 * A bare parameter of the handler the route registration hands in is an
 * object the framework passed it (its request, its reply, its context), never
 * a body: a call that forwards one sends through it, and states no body by its
 * own arguments. The row abstains. A parameter of any other function is still
 * a payload, wherever its type is declared: a callback's
 * (`.then((account) => reply.send(account))`) or a helper's
 * (`sendAccount(reply, account: Account)`), typed by the repo or by an
 * installed package. A direct send keeps the type of what it sends.
 *
 * No library, framework or member name is matched.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
/** A server library whose route registration takes the request shape as a
 * type argument and hands the handler a request and a reply built from it. */
const SERVER_KIT_DTS = `export interface RouteShape {
  Querystring?: unknown;
  Params?: unknown;
}
export interface Request<G extends RouteShape = RouteShape> {
  query: G['Querystring'];
  params: G['Params'];
  header(name: string): string | undefined;
}
export interface Reply<G extends RouteShape = RouteShape> {
  code(status: number): Reply<G>;
  type(contentType: string): Reply<G>;
  send(body?: unknown): Reply<G>;
}
export interface Server {
  get<G extends RouteShape = RouteShape>(
    path: string,
    handler: (request: Request<G>, reply: Reply<G>) => unknown
  ): void;
}
`;
/** A data library whose model types live in an installed package, as an ORM
 * or a generated client's do. */
const DATA_KIT_DTS = `export interface Account {
  id: string;
  email: string;
}
export declare function loadAccount(): Promise<Account>;
`;
const SERVICE_TS = `import type { Server, Reply } from "server-kit";
import { loadAccount, type Account } from "data-kit";

declare const app: Server;

type Item = { id: string; title: string };
declare function loadItem(): Promise<Item>;

type CallbackRequest = { query: { code?: string } };
type CallbackReply = { type: (t: string) => { send: (body: string) => unknown } };

async function finish(request: CallbackRequest, reply: CallbackReply) {
  return reply.type("text/html").send("<html></html>");
}

app.get<{ Querystring: { code?: string } }>("/delegated", async (request, reply) => {
  return finish(request, reply);
});

app.get<{ Params: { id: string }; Querystring: { code?: string } }>("/delegated-params", async (request, reply) => {
  return finish(request, reply);
});

app.get<{ Querystring: { code?: string } }>("/direct", async (request, reply) => {
  return reply.send({ ok: true });
});

app.get("/then", async (request, reply) => {
  return loadItem().then((item) => reply.send(item));
});

app.get("/account", async (request, reply) => {
  return loadAccount().then((account) => reply.send(account));
});

function sendAccount(reply: Reply, account: Account) {
  return reply.send(account);
}

app.get("/account-helper", async (request, reply) => {
  return sendAccount(reply, await loadAccount());
});

app.get<{ Querystring: { code?: string } }>("/returns-reply", async (request, reply) => {
  reply.type("text/html");
  return reply;
});
`;
const lineOf = (text) => {
    const at = SERVICE_TS.indexOf(text);
    assert.ok(at >= 0, `fixture must contain: ${text}`);
    assert.strictEqual(SERVICE_TS.indexOf(text, at + 1), -1, `fixture must contain exactly one: ${text}`);
    return SERVICE_TS.slice(0, at).split('\n').length;
};
const collapse = (text) => text.replace(/\s+/g, ' ').trim();
describe('carrick-cloud#2576: a call that forwards the handler\'s framework objects states no body', () => {
    let client;
    let repoDir;
    let servicePath;
    before(async () => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2576-'));
        fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
            compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022', lib: ['es2022'] },
            include: ['src'],
        }));
        for (const [name, dts] of [['server-kit', SERVER_KIT_DTS], ['data-kit', DATA_KIT_DTS]]) {
            const dir = path.join(repoDir, 'node_modules', name);
            fs.mkdirSync(dir, { recursive: true });
            fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({ name, version: '1.0.0', types: 'index.d.ts' }));
            fs.writeFileSync(path.join(dir, 'index.d.ts'), dts);
        }
        servicePath = path.join(repoDir, 'src', 'service.ts');
        fs.writeFileSync(servicePath, SERVICE_TS);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init-2576', repo_root: repoDir });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    /** The response read the scanner sends for a model-located send: the
     * registration line, with the located expression and its own line. */
    const infer = async (alias, registration, expression, at) => {
        const response = await client.send({
            action: 'infer',
            request_id: `infer-${alias}`,
            requests: [
                {
                    file_path: servicePath,
                    line_number: lineOf(registration),
                    expression_text: expression,
                    expression_line: lineOf(at),
                    infer_kind: 'response_body',
                    alias,
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === alias);
        return inferred ? collapse(inferred.type_string) : undefined;
    };
    const abstains = (text) => text === undefined || text === 'unknown';
    it('does not publish the request object a delegating handler forwards', async () => {
        const text = await infer('Delegated', '"/delegated", async', 'finish(request, reply)', 'return finish(request, reply);\n});\n\napp.get<{ Params');
        assert.ok(!/Querystring|Request|code\?/.test(text ?? ''), `request type leaked into the response: ${text}`);
        assert.ok(abstains(text), `expected no body, got: ${text}`);
    });
    it('does not publish the route type argument when it names path parameters too', async () => {
        const text = await infer('DelegatedParams', '"/delegated-params", async', 'finish(request, reply)', 'return finish(request, reply);\n});\n\napp.get<{ Querystring: { code?: string } }>("/direct"');
        assert.ok(!/Querystring|Params|Request/.test(text ?? ''), `request type leaked into the response: ${text}`);
        assert.ok(abstains(text), `expected no body, got: ${text}`);
    });
    it('does not publish the reply a handler returns', async () => {
        const text = await infer('ReturnsReply', '"/returns-reply", async', 'reply', 'return reply;');
        assert.ok(!/Querystring|Reply/.test(text ?? ''), `reply type leaked into the response: ${text}`);
        assert.ok(abstains(text), `expected no body, got: ${text}`);
    });
    it('keeps the type of what a direct send sends', async () => {
        const text = await infer('Direct', '"/direct", async', 'reply.send({ ok: true })', 'return reply.send({ ok: true });');
        assert.strictEqual(text, '{ ok: boolean; }');
    });
    it('keeps a repo-typed parameter handed to the send as the payload', async () => {
        const text = await infer('Then', '"/then", async', 'reply.send(item)', 'return loadItem().then((item) => reply.send(item));');
        assert.ok(text !== undefined && /\bid: string\b/.test(text) && /\btitle: string\b/.test(text), `expected the item, got: ${text}`);
    });
    it('keeps a callback parameter typed by an installed package as the payload', async () => {
        const text = await infer('Account', '"/account", async', 'reply.send(account)', 'return loadAccount().then((account) => reply.send(account));');
        assert.strictEqual(text, 'Account');
    });
    it('keeps a helper parameter typed by an installed package as the payload', async () => {
        const text = await infer('AccountHelper', 'function sendAccount', 'reply.send(account)', 'return reply.send(account);\n}');
        assert.strictEqual(text, 'Account');
    });
    it('does not publish the reply a handler returns at a line-only read', async () => {
        const response = await client.send({
            action: 'infer',
            request_id: 'infer-ReturnsReplyLine',
            requests: [{ file_path: servicePath, line_number: lineOf('"/returns-reply", async'), infer_kind: 'response_body', alias: 'ReturnsReplyLine' }],
        });
        const text = response.inferred_types?.find((t) => t.alias === 'ReturnsReplyLine')?.type_string;
        assert.ok(!/Querystring|Reply/.test(text ?? ''), `reply type leaked into the response: ${text}`);
    });
});
