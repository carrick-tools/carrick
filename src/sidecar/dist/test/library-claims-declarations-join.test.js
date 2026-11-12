/**
 * carrick#2654: a verified verdict's `declarations` join the position row of
 * the call they judge. A verdict names the declaration the call row names for
 * the call's member symbol: for a member declared as a property and typed by
 * an interface with call signatures, the property's own declaration (not the
 * signatures' lines in the interface); for a method-declared member, the
 * method. Verified ops and verified scopes follow the one rule.
 *
 * The package is invented: a class with property-typed members (`get`,
 * `mount`, each typed by an interface with overloaded call signatures) beside
 * method-declared ones (`post`, `mountOn`).
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import { SidecarClient } from './helpers.js';
const SERVER = `export type Handler = (request: unknown, reply: unknown) => void;
export interface Route {
  (path: string, handler: Handler): Server;
  (path: string, options: object, handler: Handler): Server;
}
export interface Mounter {
  (prefix: string, child: Server): Server;
  (prefix: string, child: Server, options: object): Server;
}
export declare class Server {
  constructor();
  get: Route;
  post(path: string, handler: Handler): this;
  mount: Mounter;
  mountOn(prefix: string, child: Server): this;
}
`;
const SERVICE = `import { Server } from 'fixture-server';
const app = new Server();
app.get('/a', () => {});
app.post('/b', () => {});
app.mount('/c', app);
app.mountOn('/d', app);
`;
describe('verify_library_claims: declarations join the call row (carrick#2654)', () => {
    let root;
    let store;
    let client;
    let rows;
    let verdicts;
    const P = 'fixture-server';
    const check = (id, receiver, claim) => ({
        claim_id: id,
        package: P,
        export: 'Server',
        role: 'server_framework',
        receiver,
        claim,
    });
    const op = (member) => ({ kind: 'op', op: 'receive', member, name: { arg: 0 }, of: 'instance:new' });
    const scope = (member) => ({ kind: 'scope', member, name: { arg: 0 }, mounts: { arg: 1 }, of: 'instance:new' });
    const callRow = (member) => {
        const found = rows.filter(row => row[0] === 'call' && row[3] === member);
        assert.strictEqual(found.length, 1, `one call row for ${member}`);
        return found[0];
    };
    const joined = (verdict, member) => {
        const row = callRow(member);
        assert.strictEqual(verdict.verdict, 'verified', JSON.stringify(verdict));
        assert.deepStrictEqual(verdict.declarations, [{ decl_file: row[4], decl_line: row[5] }], member);
    };
    before(async () => {
        root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-claims-join-')));
        const write = (rel, text) => {
            const target = path.join(root, rel);
            fs.mkdirSync(path.dirname(target), { recursive: true });
            fs.writeFileSync(target, text);
        };
        write('tsconfig.json', JSON.stringify({
            compilerOptions: { target: 'es2020', module: 'commonjs', moduleResolution: 'node', strict: true, skipLibCheck: true, types: [] },
            include: ['src/**/*.ts'],
        }));
        write('package.json', JSON.stringify({ name: 'service' }));
        write('src/index.ts', SERVICE);
        write('node_modules/fixture-server/package.json', JSON.stringify({ name: P, version: '1.0.0', types: 'index.d.ts' }));
        write('node_modules/fixture-server/index.d.ts', SERVER);
        client = new SidecarClient();
        await client.start();
        const ready = await client.send({
            request_id: 'init',
            action: 'init',
            repo_root: root,
            tsconfig_path: 'tsconfig.json',
            scan_root: root,
        });
        assert.strictEqual(ready.status, 'ready');
        store = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-claims-join-rows-')));
        const out = path.join(store, 'rows.jsonl.gz');
        const walked = await client.send({ action: 'position_rows', request_id: 'rows', files: [path.join(root, 'src/index.ts')], scan_root: root, out_path: out, service: 'svc' }, 60_000);
        assert.strictEqual(walked.status, 'success', JSON.stringify(walked.errors));
        const lines = zlib.gunzipSync(fs.readFileSync(out)).toString('utf8').trimEnd().split('\n').slice(1);
        rows = JSON.parse(lines[0]).r;
        const response = await client.send({
            request_id: 'claims',
            action: 'verify_library_claims',
            from_dir: root,
            scan_root: root,
            checks: [
                check('m0', 'export', { kind: 'make', form: 'new', member: null }),
                check('get', 'instance:new', op('get')),
                check('post', 'instance:new', op('post')),
                check('mount', 'instance:new', scope('mount')),
                check('mountOn', 'instance:new', scope('mountOn')),
            ],
        }, 60_000);
        assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
        verdicts = response.verdicts;
    });
    after(async () => {
        await client.stop();
        fs.rmSync(root, { recursive: true, force: true });
        fs.rmSync(store, { recursive: true, force: true });
    });
    it('a property typed by call signatures names the property, as its call row does', () => {
        joined(verdicts[1], 'get');
        assert.strictEqual(verdicts[1].declarations[0].decl_line, SERVER.split('\n').findIndex(l => l.includes('get: Route')) + 1);
    });
    it('a method-declared member names the method, as its call row does', () => {
        joined(verdicts[2], 'post');
    });
    it('a verified scope follows the same rule, property-typed and method-declared', () => {
        joined(verdicts[3], 'mount');
        joined(verdicts[4], 'mountOn');
    });
});
