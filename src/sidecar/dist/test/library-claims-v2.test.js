/**
 * carrick#2434: label structure v2 on `verify_library_claims`. Each field the
 * store's rebuild writes is checked against the package's own declarations,
 * one valid claim and one invalid claim per field:
 * - `method` on a route member (the member's own name is the method) and
 *   `ALL` on a route member that registers every method;
 * - `method_key` (where the method sits) and `method_default` (the method
 *   when the option is absent, or the one method a member sends);
 * - an op's `base`;
 * - `runs` on a maker (a hook runs the function it is given) and
 *   `runs: { bound: 'maker' }` on what that maker returns (`mutate` runs the
 *   function its maker was given);
 * - `yields` on a receive op (a server's connection handler is handed the
 *   server end of a socket);
 * - `server_framework`, read since this structure.
 *
 * Every package is invented and its declarations are hand-written from the
 * general shape (a client factory with a base URL option, verb members, a
 * request member whose method is an option, a router type, a hook that
 * returns `{ mutate }`, a socket class a server's connection event hands
 * over), never copied from a real package. Claims with none of these fields
 * are pinned unchanged by `library-claims.test.ts` and
 * `library-claims-http-parity.test.ts`.
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
// An HTTP client: a factory taking a base URL, verb members, a member whose
// method is an option, one that always posts, one that takes its base per call.
const HTTP = `export type Method = 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE';
export interface RequestInit {
  method?: Method;
  body?: unknown;
  mode?: string;
  headers?: Record<string, string>;
}
export interface HttpClient {
  get(path: string, init?: { headers?: Record<string, string> }): Promise<unknown>;
  request(path: string, init?: RequestInit): Promise<unknown>;
  beacon(path: string, data?: unknown): boolean;
  from(base: string, path: string): Promise<unknown>;
}
export interface Static {
  create(options: { baseUrl: string; timeout?: number }): HttpClient;
}
declare const http: Static;
export default http;
`;
// A hook that runs the function it is given, and one that returns an object
// whose member runs the function its maker was given.
const HOOKS = `export interface QueryOptions<T> {
  queryKey: string[];
  queryFn: () => Promise<T>;
}
export interface QueryResult<T> {
  data: T | undefined;
}
export declare function useQuery<T = unknown>(options: QueryOptions<T>): QueryResult<T>;
export interface MutationResult<TInput> {
  mutate(input: TInput): void;
  isPending: boolean;
}
export declare function useMutation<TInput = unknown>(fn: (input: TInput) => Promise<unknown>): MutationResult<TInput>;
`;
// A router: each verb member registers that method at a path.
const ROUTER = `export type Handler = (request: unknown, response: unknown) => void;
export interface Router {
  get(path: string, ...handlers: Handler[]): this;
  post(path: string, ...handlers: Handler[]): this;
  all(path: string, ...handlers: Handler[]): this;
  use(...handlers: Handler[]): this;
}
export declare function router(): Router;
`;
// A socket class used on both ends: constructed with a URL by a client, and
// handed over by a server's connection event.
const SOCKETS = `export declare class Socket {
  constructor(url: string);
  send(data: string): void;
  on(event: 'message', listener: (data: string) => void): this;
}
export declare class Peer {
  close(): void;
}
export declare class SocketServer {
  constructor(options?: { port?: number });
  on(event: 'connection', listener: (socket: Socket, peer: Peer) => void): this;
}
`;
function writeTree(root, files) {
    for (const [rel, text] of Object.entries(files)) {
        const file = path.join(root, rel);
        fs.mkdirSync(path.dirname(file), { recursive: true });
        fs.writeFileSync(file, text);
    }
}
const packageJson = (name) => JSON.stringify({ name, version: '1.0.0', types: 'index.d.ts' });
let sequence = 0;
const check = (pkg, exported, role, receiver, claim) => ({
    claim_id: `v${sequence++}`,
    package: pkg,
    export: exported,
    role,
    receiver,
    claim,
});
const make = (form, member, parts = {}) => ({
    kind: 'make',
    form,
    member,
    ...parts,
});
const op = (opKind, member, parts) => ({
    kind: 'op',
    op: opKind,
    member,
    ...parts,
});
function verdicts(response) {
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    return response.verdicts.map(r => (r.verdict === 'verified' ? 'verified' : `${r.verdict} ${r.reason}`));
}
describe('verify_library_claims: label structure v2 (carrick#2434)', () => {
    let root;
    let client;
    let requestId = 0;
    const send = (checks) => client.send({ request_id: `v2-${requestId++}`, action: 'verify_library_claims', from_dir: root, checks }, 60_000);
    before(async () => {
        root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-claims-v2-'));
        writeTree(root, {
            'tsconfig.json': TSCONFIG,
            'package.json': JSON.stringify({ name: 'service' }),
            'src/index.ts': 'export const service = 1;\n',
            'node_modules/fixture-http/package.json': packageJson('fixture-http'),
            'node_modules/fixture-http/index.d.ts': HTTP,
            'node_modules/fixture-hooks/package.json': packageJson('fixture-hooks'),
            'node_modules/fixture-hooks/index.d.ts': HOOKS,
            'node_modules/fixture-router/package.json': packageJson('fixture-router'),
            'node_modules/fixture-router/index.d.ts': ROUTER,
            'node_modules/fixture-sockets/package.json': packageJson('fixture-sockets'),
            'node_modules/fixture-sockets/index.d.ts': SOCKETS,
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
    const H = 'fixture-http';
    const factory = check(H, 'default', 'http_client', 'export', make('call', 'create', { base: { arg: 0, key: 'baseUrl' } }));
    const onClient = (claim) => check(H, 'default', 'http_client', 'instance:create', { ...claim, of: 'instance:create' });
    it('method_key with method_default: the method is an option, GET when absent', async () => {
        const request = (parts) => onClient(op('request', 'request', { name: { arg: 0 }, key_labels: { mode: 'not_name' }, ...parts }));
        assert.deepStrictEqual(verdicts(await send([
            factory,
            request({ method_key: { arg: 1, key: 'method' }, method_default: 'GET', payload: { arg: 1, key: 'body' } }),
            // Invalid: the key named is not where a method can sit.
            request({ method_key: { arg: 1, key: 'headers' }, method_default: 'GET' }),
            // Invalid: the default is no method word.
            request({ method_key: { arg: 1, key: 'method' }, method_default: 'FETCH' }),
            // Invalid: a fixed method and a default say two things.
            request({ method: 'REQUEST', method_default: 'GET' }),
        ])), ['verified', 'verified', 'failed method_not_string', 'failed method_not_word', 'unchecked claim_invalid']);
    });
    it('method_default alone: the one method a member sends, whatever it is called', async () => {
        const beacon = (parts) => onClient(op('request', 'beacon', { name: { arg: 0 }, payload: { arg: 1 }, ...parts }));
        assert.deepStrictEqual(verdicts(await send([factory, beacon({ method_default: 'POST' }), beacon({ method_default: 'SEND' })])), ['verified', 'verified', 'failed method_not_word']);
    });
    it('a verb member under v2: its own name is its method', async () => {
        const get = (parts) => onClient(op('request', 'get', { name: { arg: 0 }, ...parts }));
        assert.deepStrictEqual(verdicts(await send([factory, get({ method: 'GET' }), get({ method: 'POST' }), onClient(op('request', 'beacon', { name: { arg: 0 }, payload: { arg: 1 }, method_default: 'POST' }))])), ['verified', 'verified', 'failed method_not_member_verb', 'verified']);
    });
    it('an op base: where the base URL the path joins sits', async () => {
        const from = (parts) => onClient(op('request', 'from', { method_default: 'GET', ...parts }));
        assert.deepStrictEqual(verdicts(await send([
            factory,
            from({ base: { arg: 0 }, name: { arg: 1 } }),
            // Invalid: no argument there.
            from({ base: { arg: 4 }, name: { arg: 1 } }),
            // Invalid: the base and the path read one argument.
            from({ base: { arg: 1 }, name: { arg: 1 } }),
        ])), ['verified', 'verified', 'failed param_missing', 'failed slots_overlap']);
    });
    it('runs on a maker: a hook runs the function it is given, never a key that is data', async () => {
        const K = 'fixture-hooks';
        assert.deepStrictEqual(verdicts(await send([
            check(K, 'useQuery', 'http_client', 'export', make('call', null, { runs: { arg: 0, key: 'queryFn' } })),
            check(K, 'useQuery', 'http_client', 'export', make('call', null, { runs: { arg: 0, key: 'queryKey' } })),
            check(K, 'useMutation', 'http_client', 'export', make('call', null, { runs: { arg: 0 } })),
            check(K, 'useMutation', 'http_client', 'export', make('call', null, { runs: { arg: 1 } })),
        ])), ['verified', 'failed runs_not_function', 'verified', 'failed param_missing']);
    });
    it('runs bound to the maker: what the hook returns runs the function the hook was given', async () => {
        const K = 'fixture-hooks';
        const maker = check(K, 'useMutation', 'http_client', 'export', make('call', null, { runs: { arg: 0 } }));
        const mutate = (receiver, runs, place) => check(K, 'useMutation', 'http_client', receiver, op('request', 'mutate', { runs, ...place }));
        assert.deepStrictEqual(verdicts(await send([
            maker,
            mutate('instance:()', { bound: 'maker', arg: 0 }, { of: 'instance:()' }),
            // Invalid: the maker takes nothing there.
            mutate('instance:()', { bound: 'maker', arg: 2 }, { of: 'instance:()' }),
            // Invalid: read on the export, no maker made it.
            mutate('export', { bound: 'maker', arg: 0 }, { on: 'export' }),
        ])), ['verified', 'verified', 'failed param_missing', 'failed runs_unbound']);
    });
    it('server_framework: a route member registers the method its name says at the path', async () => {
        const R = 'fixture-router';
        const route = (member, parts) => check(R, 'router', 'server_framework', 'instance:()', op('receive', member, { name: { arg: 0 }, of: 'instance:()', ...parts }));
        assert.deepStrictEqual(verdicts(await send([
            check(R, 'router', 'server_framework', 'export', make('call', null)),
            route('get', { method: 'GET' }),
            route('all', { method: 'ALL' }),
            // Invalid: the member's name is another method.
            route('post', { method: 'GET' }),
            // Invalid: a router registers routes, it sends nothing.
            check(R, 'router', 'server_framework', 'instance:()', op('request', 'get', { name: { arg: 0 }, method: 'GET', of: 'instance:()' })),
        ])), ['verified', 'verified', 'verified', 'failed method_not_member_verb', 'unchecked role_unsupported']);
    });
    it('yields: a connection handler is handed the server end of the package’s own socket', async () => {
        const S = 'fixture-sockets';
        const server = check(S, 'SocketServer', 'socket', 'export', make('new', null));
        const connection = (yields, parts = {}) => check(S, 'SocketServer', 'socket', 'instance:new', 
        // The event is the library's own (`connection`): the op names nothing.
        op('receive', 'on', { handler: { arg: 1 }, of: 'instance:new', yields, ...parts }));
        assert.deepStrictEqual(verdicts(await send([
            server,
            connection({ param: 0, export: 'Socket', side: 'server' }),
            connection({ param: 1, export: 'Peer', side: 'server' }),
            // Invalid: the parameter there is another export.
            connection({ param: 1, export: 'Socket', side: 'server' }),
            // Invalid: no such side.
            connection({ param: 0, export: 'Socket', side: 'middle' }),
            // Invalid: yields belongs to a handler a receive op takes.
            check(S, 'SocketServer', 'socket', 'instance:new', op('send', 'on', { name: { arg: 0 }, payload: { arg: 1 }, of: 'instance:new', yields: { param: 0, export: 'Socket', side: 'server' } })),
        ])), ['verified', 'verified', 'verified', 'failed yields_not_export', 'failed yields_side_invalid', 'unchecked claim_invalid']);
    });
    it('a message role carrying an HTTP field is invalid, as before', async () => {
        const S = 'fixture-sockets';
        assert.deepStrictEqual(verdicts(await send([
            check(S, 'SocketServer', 'socket', 'export', make('new', null)),
            check(S, 'SocketServer', 'socket', 'instance:new', op('receive', 'on', { name: { arg: 0 }, handler: { arg: 1 }, of: 'instance:new', method_default: 'GET' })),
        ])), ['verified', 'unchecked claim_invalid']);
    });
    it('refuses a request whose v2 field is outside the shape', async () => {
        const shapes = [
            op('request', 'get', { name: { arg: 0 }, runs: { bound: 'scope', arg: 0 } }),
            op('receive', 'on', { name: { arg: 0 }, yields: { param: -1, export: 'Socket', side: 'server' } }),
            op('request', 'get', { name: { arg: 0 }, method_default: '' }),
            make('call', null, { runs: { bound: 'maker', arg: 0 } }),
        ];
        for (const claim of shapes) {
            const response = await send([check(H, 'default', 'http_client', 'export', claim)]);
            assert.strictEqual(response.status, 'error', JSON.stringify(claim));
        }
    });
});
