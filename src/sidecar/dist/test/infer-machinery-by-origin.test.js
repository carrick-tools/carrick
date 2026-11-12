/**
 * Blueprint row N8: whether a type is framework machinery is decided by where
 * it is declared, never by its name.
 *
 * - A type declared by the platform's library (`Response`) that carries the
 *   transport's members is machinery (`typeIsFrameworkMachinery`).
 * - A type declared by an installed package the store labels a server
 *   framework (`framework_packages` on the infer request) is machinery,
 *   whatever members it has.
 * - A service's own `Context`, `Request` or `Router` is its contract.
 *
 * `machinery` marks the type `type_string` prints; `anchor_machinery` marks
 * the type `primary_type_symbol` names.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import * as fs from 'node:fs';
import * as os from 'node:os';
import { SidecarClient } from './helpers.js';
const APP_TS = `import { Reply, server } from 'fixture-server';

export interface Context { id: string; tenant: string }
export interface Request { orderId: string; quantity: number }
export interface Router { routes: string[] }

declare function load<T>(): T;

export const ctx: Context = load<Context>();
export const req: Request = load<Request>();
export const router: Router = load<Router>();
export const reply: Reply = server.reply();

export async function call() {
  const res: Response = await fetch('http://orders.internal/orders');
  return res;
}

export const pending = fetch('http://orders.internal/orders');
export const loaded = load<Context>();
`;
const SERVER_DTS = `export interface Reply {
  code(status: number): Reply;
  header(name: string, value: string): Reply;
}
export declare const server: { reply(): Reply };
`;
function spanOf(text) {
    const start = APP_TS.indexOf(text);
    assert.ok(start >= 0, `fixture must contain: ${text}`);
    return { start, end: start + text.length, line: APP_TS.slice(0, start).split('\n').length };
}
describe('framework machinery is decided by where a type is declared (N8)', () => {
    let client;
    let repo;
    let appFile;
    before(async () => {
        repo = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-machinery-origin-'));
        fs.mkdirSync(path.join(repo, 'src'));
        fs.writeFileSync(path.join(repo, 'package.json'), JSON.stringify({ name: 'orders-api', version: '1.0.0', dependencies: { 'fixture-server': '1.0.0' } }));
        fs.writeFileSync(path.join(repo, 'tsconfig.json'), JSON.stringify({
            compilerOptions: {
                target: 'es2022',
                module: 'esnext',
                moduleResolution: 'bundler',
                lib: ['es2022', 'dom'],
                strict: true,
                skipLibCheck: true,
            },
            include: ['src/**/*'],
        }));
        const pkg = path.join(repo, 'node_modules', 'fixture-server');
        fs.mkdirSync(pkg, { recursive: true });
        fs.writeFileSync(path.join(pkg, 'package.json'), JSON.stringify({ name: 'fixture-server', version: '1.0.0', types: 'index.d.ts' }));
        fs.writeFileSync(path.join(pkg, 'index.d.ts'), SERVER_DTS);
        appFile = path.join(repo, 'src', 'app.ts');
        fs.writeFileSync(appFile, APP_TS);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'machinery-origin-init', repo_root: repo });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repo, { recursive: true, force: true });
    });
    async function infer(text, kind, frameworkPackages) {
        const at = spanOf(text);
        const alias = `Alias${at.start}`;
        const response = await client.send({
            action: 'infer',
            request_id: `machinery-origin-${at.start}-${frameworkPackages ? 'store' : 'none'}`,
            requests: [
                {
                    file_path: appFile,
                    line_number: at.line,
                    span_start: at.start,
                    span_end: at.end,
                    infer_kind: kind,
                    alias,
                },
            ],
            ...(frameworkPackages ? { framework_packages: frameworkPackages } : {}),
        });
        const inferred = response.inferred_types?.find((t) => t.alias === alias);
        assert.ok(inferred, `expected an inference for ${text}: ${JSON.stringify(response.errors)}`);
        return inferred;
    }
    it("keeps a service's own Context, Request and Router as contracts", async () => {
        for (const [text, name] of [
            ['ctx: Context = load<Context>()', 'Context'],
            ['req: Request = load<Request>()', 'Request'],
            ['router: Router = load<Router>()', 'Router'],
        ]) {
            const inferred = await infer(text, 'variable', ['fixture-server']);
            assert.strictEqual(inferred.type_string, name);
            assert.strictEqual(inferred.machinery, undefined, `${name} is the service's own type`);
        }
    });
    it('marks a type a store-labelled server framework declares', async () => {
        const inferred = await infer('reply: Reply = server.reply()', 'variable', ['fixture-server']);
        assert.strictEqual(inferred.type_string, 'Reply');
        assert.strictEqual(inferred.machinery, true);
    });
    it('does not mark the same type when the store labels no server framework', async () => {
        const inferred = await infer('reply: Reply = server.reply()', 'variable');
        assert.strictEqual(inferred.machinery, undefined);
    });
    it("marks the platform library's Response by its declaration and members", async () => {
        const inferred = await infer("res: Response = await fetch('http://orders.internal/orders')", 'variable');
        assert.strictEqual(inferred.type_string, 'Response');
        assert.strictEqual(inferred.machinery, true);
    });
    it('marks an anchor on machinery, and never an anchor on a user type', async () => {
        const transport = await infer("fetch('http://orders.internal/orders')", 'expression');
        assert.strictEqual(transport.primary_type_symbol, 'Response');
        assert.strictEqual(transport.anchor_machinery, true);
        const own = await infer('load<Context>()', 'expression');
        assert.strictEqual(own.primary_type_symbol, 'Context');
        assert.strictEqual(own.anchor_machinery, undefined);
    });
});
