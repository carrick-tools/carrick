/**
 * carrick#2474 R6a: procedure-router fields on `verify_library_claims`
 * (contract amendment 3 of 2026-10-23 on carrick#2434). Each field is checked
 * against the package's own declarations, one valid claim and one invalid
 * claim per field:
 * - `collects` on a scope: the router member takes an object whose keys
 *   name procedures and nested routers;
 * - `name: { bound: 'key' }` on a receive op: the procedure is named by the
 *   key it sits under, and the label states its method;
 * - `merges` on a scope (amendment 9): the member takes, as a rest, the
 *   routers a verified `collects` scope returns;
 * - an adapter, a maker whose `runs` slot takes the router a `collects`
 *   scope returns, with its endpoint at `prefix`.
 *
 * The packages are invented and hand-written from the general shape of a
 * procedure kit (a kit made by `init.create()`, a procedure builder whose
 * `query` and `mutation` members register a resolver, a `router` member
 * taking an object of procedures, a fetch-style adapter taking the router
 * and an endpoint) and of a second package that ships a middleware adapter
 * for it, never copied from a real one. Claims with none of these fields are
 * pinned unchanged by `library-claims.test.ts`, `library-claims-v2.test.ts`
 * and `library-claims-routes.test.ts`.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const TSCONFIG = JSON.stringify({
    compilerOptions: {
        target: 'es2020',
        module: 'commonjs',
        moduleResolution: 'node',
        strict: true,
        esModuleInterop: true,
        skipLibCheck: true,
        types: [],
    },
    include: ['src/**/*.ts'],
});
// A procedure kit: `init.create()` makes a kit whose builder registers
// resolvers and whose `router` member collects them by key. `label` takes a
// string, `loose` an untyped record. The adapters serve a router at an
// endpoint; `serveOther` takes something no router fits, `serveLoose` says
// nothing about what it takes.
const PROCS = `export interface QueryProcedure<O> {
  readonly kind: 'query';
  readonly output: O;
}
export interface MutationProcedure<O> {
  readonly kind: 'mutation';
  readonly output: O;
}
export type AnyProcedure = QueryProcedure<unknown> | MutationProcedure<unknown>;
export interface RouterRecord {
  [key: string]: AnyProcedure | RouterRecord | Router<RouterRecord>;
}
export interface Router<R extends RouterRecord = RouterRecord> {
  readonly record: R;
  readonly isRouter: true;
}
export interface ProcedureBuilder {
  input<S>(schema: S): ProcedureBuilder;
  query<O>(resolver: (options: { input: unknown }) => O): QueryProcedure<O>;
  mutation<O>(resolver: (options: { input: unknown }) => O): MutationProcedure<O>;
}
export interface Kit {
  procedure: ProcedureBuilder;
  router<R extends RouterRecord>(record: R): Router<R>;
  label(name: string): Kit;
  loose(record: any): Router;
  mergeRouters<R extends Router[]>(...routers: [...R]): Router;
  mergeNone(...names: string[]): Router;
  mergeLoose(...anything: any[]): Router;
  mergeLists(...lists: Router[][]): Router;
  mergeArray(routers: Router[]): Router;
  mergeArrayOf<R extends Router[]>(routers: R): Router;
  mergePlain(first: Router, opts: { x: string }): Router;
  mergeRest(...routers: Router[]): Router;
  mergeRestOf<R extends Router[]>(...routers: R): Router;
}
export interface Init {
  create(): Kit;
}
export declare const init: Init;
export interface Reply {
  status: number;
}
export declare function serveRpc<R extends Router>(options: { router: R; endpoint: string }): Promise<Reply>;
export declare function serveOther(options: { router: { other: number }; endpoint: string }): Promise<Reply>;
export declare function serveLoose(options: { router: any; endpoint: string }): Promise<Reply>;
export declare function serveFn(options: { router: (request: unknown) => void; endpoint: string }): Promise<Reply>;
`;
// A second package, an HTTP framework's middleware adapter for the kit's
// routers: the router it serves is declared by the other package.
const MIDDLEWARE = `import type { Router } from 'fixture-procs';
export type Middleware = (request: unknown, response: unknown, next: () => void) => void;
export declare function rpcMiddleware(options: { router: Router }): Middleware;
`;
function writeTree(root, files) {
    for (const [rel, text] of Object.entries(files)) {
        const file = path.join(root, rel);
        fs.mkdirSync(path.dirname(file), { recursive: true });
        fs.writeFileSync(file, text);
    }
}
const P = 'fixture-procs';
let sequence = 0;
const check = (exported, receiver, claim, pkg = P) => ({
    claim_id: `p${sequence++}`,
    package: pkg,
    export: exported,
    role: 'server_framework',
    receiver,
    claim,
});
const KIT = 'instance:create';
const maker = check('init', 'export', { kind: 'make', form: 'call', member: 'create' });
const onKit = (claim) => check('init', KIT, { ...claim, of: KIT });
const scope = (member, parts) => ({ kind: 'scope', member, ...parts });
const router = (parts = {}) => onKit(scope('router', { collects: { arg: 0, join: '.' }, ...parts }));
const procedure = (member, parts) => onKit({ kind: 'op', op: 'receive', member, path: ['procedure'], name: { bound: 'key' }, handler: { arg: 0 }, ...parts });
const adapter = (exported, parts, pkg = P) => check(exported, 'export', { kind: 'make', form: 'call', member: null, ...parts }, pkg);
const serves = { runs: { arg: 0, key: 'router' }, prefix: { arg: 0, key: 'endpoint' } };
function verdicts(response) {
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    return response.verdicts.map(r => (r.verdict === 'verified' ? 'verified' : `${r.verdict} ${r.reason}`));
}
describe('verify_library_claims: procedure-router fields (carrick#2474 R6a)', () => {
    let root;
    let client;
    let requestId = 0;
    const run = async (checks) => verdicts(await client.send({ request_id: `procs-${requestId++}`, action: 'verify_library_claims', from_dir: root, checks }, 60_000));
    // The kit's maker leads; its verdict is dropped.
    const send = async (checks) => (await run([maker, ...checks])).slice(1);
    before(async () => {
        root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-claims-procs-'));
        writeTree(root, {
            'tsconfig.json': TSCONFIG,
            'package.json': JSON.stringify({ name: 'service' }),
            'src/index.ts': 'export const service = 1;\n',
            'node_modules/fixture-procs/package.json': JSON.stringify({ name: P, version: '1.0.0', types: 'index.d.ts' }),
            'node_modules/fixture-procs/index.d.ts': PROCS,
            'node_modules/fixture-rpc-middleware/package.json': JSON.stringify({
                name: 'fixture-rpc-middleware',
                version: '1.0.0',
                types: 'index.d.ts',
            }),
            'node_modules/fixture-rpc-middleware/index.d.ts': MIDDLEWARE,
        });
        client = new SidecarClient();
        await client.start();
        const ready = await client.send({ request_id: 'init', action: 'init', repo_root: root });
        assert.strictEqual(ready.status, 'ready');
    });
    after(async () => {
        await client.stop();
        fs.rmSync(root, { recursive: true, force: true });
    });
    it('collects: the router member takes an object of procedures', async () => {
        assert.deepStrictEqual(await send([
            router(),
            // Invalid: the member takes a string, not an object.
            onKit(scope('label', { collects: { arg: 0, join: '.' } })),
            // Says nothing: the record is untyped.
            onKit(scope('loose', { collects: { arg: 0, join: '.' } })),
            // Invalid: no argument there.
            router({ collects: { arg: 3, join: '.' } }),
        ]), ['verified', 'failed collects_not_object', 'unchecked collects_untyped', 'failed param_missing']);
    });
    it('collects pairs as the contract says: exactly one of name or collects, never beside runs, yields or mounts', async () => {
        assert.deepStrictEqual(await send([
            router({ name: { arg: 0 } }),
            router({ mounts: { arg: 0 } }),
            router({ runs: { arg: 0 }, yields: { param: 0, export: 'Kit', side: 'server' } }),
            // Neither a name nor a collection.
            onKit(scope('router', {})),
        ]), ['unchecked claim_invalid', 'unchecked claim_invalid', 'unchecked claim_invalid', 'unchecked claim_invalid']);
    });
    it('bound key: a procedure is named by its key once a collects scope verifies, with the method the label states', async () => {
        assert.deepStrictEqual(await send([
            router(),
            procedure('query', { method: 'GET' }),
            procedure('mutation', { method: 'POST' }),
            // Invalid: not a method word.
            procedure('query', { method: 'FETCH' }),
        ]), ['verified', 'verified', 'verified', 'failed method_not_word']);
    });
    it('bound key is unbound with no verified collects scope of the same package', async () => {
        // No collects scope in the request.
        assert.deepStrictEqual(await send([procedure('query', { method: 'GET' })]), ['failed name_unbound']);
        // The only collects scope fails.
        assert.deepStrictEqual(await send([onKit(scope('label', { collects: { arg: 0, join: '.' } })), procedure('query', { method: 'GET' })]), ['failed collects_not_object', 'failed name_unbound']);
    });
    it('bound key needs a stated method, a receive op and a handler', async () => {
        assert.deepStrictEqual(await send([
            router(),
            procedure('query', {}),
            procedure('query', { method: 'GET', handler: undefined }),
        ]), ['verified', 'unchecked claim_invalid', 'unchecked claim_invalid']);
    });
    it('an adapter runs the router a collects scope returns, and its endpoint is the prefix', async () => {
        assert.deepStrictEqual(await send([
            router(),
            adapter('serveRpc', serves),
            // Another package's adapter serves this package's router.
            adapter('rpcMiddleware', { runs: { arg: 0, key: 'router' } }, 'fixture-rpc-middleware'),
            // Invalid: no router fits what it takes.
            adapter('serveOther', serves),
            // Says nothing: what it takes is untyped.
            adapter('serveLoose', serves),
        ]), ['verified', 'verified', 'verified', 'failed runs_not_collection', 'unchecked runs_untyped']);
    });
    it('an adapter is read as today with no verified collects scope in the request', async () => {
        assert.deepStrictEqual(await send([
            adapter('serveRpc', serves),
            adapter('serveFn', serves),
            adapter('serveLoose', serves),
        ]), ['failed runs_not_function', 'verified', 'unchecked runs_untyped']);
    });
    it('merges: the member takes routers in a rest slot beside a verified collects scope', async () => {
        const merging = (member, parts = {}) => onKit(scope(member, { merges: { arg: 0, each: true }, ...parts }));
        assert.deepStrictEqual(await send([
            router(),
            merging('mergeRouters'),
            // Invalid: the rest takes strings, no router fits.
            merging('mergeNone'),
            // Says nothing: the rest is untyped.
            merging('mergeLoose'),
        ]), ['verified', 'verified', 'failed merges_not_collection', 'unchecked merges_untyped']);
    });
    it('merges reads a rest of routers, never an array of them (out of amendment 9)', async () => {
        const merging = (member) => onKit(scope(member, { merges: { arg: 0, each: true } }));
        assert.deepStrictEqual(await send([
            router(),
            merging('mergeRest'),
            merging('mergeRestOf'),
            merging('mergeLists'),
            merging('mergeArray'),
            merging('mergeArrayOf'),
        ]), [
            'verified',
            'verified',
            'verified',
            'failed merges_not_collection',
            'failed merges_not_rest',
            'failed merges_not_rest',
        ]);
    });
    it('merges verifies only on a rest parameter, never a plain one', async () => {
        const at = (member, arg) => onKit(scope(member, { merges: { arg, each: true } }));
        assert.deepStrictEqual(await send([router(), at('mergePlain', 0), at('mergePlain', 1), at('mergePlain', 5)]), [
            'verified',
            'failed merges_not_rest',
            'failed merges_not_rest',
            'failed param_missing',
        ]);
    });
    it('merges answers a missing member before it answers unbound', async () => {
        const [outcome] = await send([onKit(scope('noSuchMerge', { merges: { arg: 0, each: true } }))]);
        assert.ok(!outcome.includes('merges_unbound'), outcome);
    });
    it('merges is unbound with no verified collects scope in the request', async () => {
        assert.deepStrictEqual(await send([onKit(scope('mergeRouters', { merges: { arg: 0, each: true } }))]), [
            'failed merges_unbound',
        ]);
    });
    it('merges pairs as the contract says: alone, and as a rest slot', async () => {
        const claims = [
            scope('mergeRouters', { merges: { arg: 0, each: true }, name: { arg: 0 } }),
            scope('mergeRouters', { merges: { arg: 0, each: true }, collects: { arg: 0, join: '.' } }),
            scope('mergeRouters', { merges: { arg: 0, each: true }, mounts: { arg: 0 } }),
            scope('mergeRouters', { merges: { arg: 0 } }),
            scope('mergeRouters', { merges: { arg: 0, each: true, key: 'k' } }),
        ];
        assert.deepStrictEqual(await send([router(), ...claims.map(onKit)]), ['verified', ...claims.map(() => 'unchecked claim_invalid')]);
    });
    it('refuses a request whose procedure field is outside the shape', async () => {
        const shapes = [
            router({ collects: { arg: 0 } }),
            router({ collects: { arg: 0, join: '' } }),
            router({ collects: { arg: -1, join: '.' } }),
            router({ collects: { arg: 0, join: '.', path: ['x'] } }),
            procedure('query', { method: 'GET', name: { bound: 'keys' } }),
        ];
        for (const shape of shapes) {
            const response = await client.send({ request_id: `procs-${requestId++}`, action: 'verify_library_claims', from_dir: root, checks: [maker, shape] }, 60_000);
            assert.strictEqual(response.status, 'error', JSON.stringify(shape.claim));
        }
    });
});
