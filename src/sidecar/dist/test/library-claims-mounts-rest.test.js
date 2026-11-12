/**
 * carrick#2661: a scope's `mounts` argument that lands on a rest parameter
 * (`use(path, ...handlers)`) is read against the rest's element type.
 *
 * The package is invented, the general shape of a server whose mount member
 * takes its routers as a rest of handlers.
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
const PACKAGE = `export type Req = { path: string };
export type Handler = (req: Req) => void;
export interface App {
  (req: Req): void;
  get(path: string, handler: Handler): this;
  use(path: string, ...handlers: Array<Handler | App>): this;
  useReadonly(path: string, ...handlers: ReadonlyArray<Handler | App>): this;
  useTuple(path: string, ...handlers: [App, Handler]): this;
  useHandlers(path: string, ...handlers: Array<(req: Req) => string>): this;
  useAny(path: string, ...handlers: any[]): this;
  useUnknown(path: string, ...handlers: unknown[]): this;
  useUnion(path: string, ...handlers: Array<Handler | unknown>): this;
  useGeneric<H>(path: string, ...handlers: H[]): this;
  useOther(path: string, ...handlers: Array<Handler | number>): this;
}
export declare function createApp(): App;
`;
describe('verify_library_claims: mounts at a rest parameter (carrick#2661)', () => {
    let root;
    let client;
    const claim = (id, member, receiver, exportName) => ({
        claim_id: id,
        package: 'fixture-rest',
        export: exportName,
        role: 'server_framework',
        receiver,
        claim: { kind: 'scope', member, name: { arg: 0 }, mounts: { arg: 1 }, of: receiver },
    });
    before(async () => {
        root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-claims-mounts-rest-'));
        const files = {
            'tsconfig.json': TSCONFIG,
            'package.json': JSON.stringify({ name: 'service' }),
            'src/index.ts': 'export const service = 1;\n',
            'node_modules/fixture-rest/package.json': JSON.stringify({ name: 'fixture-rest', version: '1.0.0', types: 'index.d.ts' }),
            'node_modules/fixture-rest/index.d.ts': PACKAGE,
        };
        for (const [rel, text] of Object.entries(files)) {
            const file = path.join(root, rel);
            fs.mkdirSync(path.dirname(file), { recursive: true });
            fs.writeFileSync(file, text);
        }
        client = new SidecarClient();
        await client.start();
        const ready = await client.send({ request_id: 'init', action: 'init', repo_root: root });
        assert.strictEqual(ready.status, 'ready');
    });
    after(async () => {
        await client.stop();
        fs.rmSync(root, { recursive: true, force: true });
    });
    it('reads the rest element type', async () => {
        const members = ['use', 'useReadonly', 'useTuple', 'useHandlers', 'useAny', 'useUnknown', 'useUnion', 'useGeneric', 'useOther'];
        const response = await client.send({
            request_id: 'mounts-rest',
            action: 'verify_library_claims',
            from_dir: root,
            checks: [
                {
                    claim_id: 'make',
                    package: 'fixture-rest',
                    export: 'createApp',
                    role: 'server_framework',
                    receiver: 'export',
                    claim: { kind: 'make', form: 'call', member: null },
                },
                ...members.map(m => claim(m, m, 'instance:()', 'createApp')),
            ],
        }, 60_000);
        assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
        const read = response.verdicts.map(v => (v.verdict === 'verified' ? 'verified' : `${v.verdict} ${v.reason}`));
        assert.deepStrictEqual(read.slice(1), [
            'verified', // Array<Handler | App>: the instance is callable and fits App
            'verified', // ReadonlyArray
            'verified', // tuple: the element at the mount argument
            'failed mounts_not_instance', // handlers only: the instance does not return string
            'unchecked mounts_untyped', // any[]
            'unchecked mounts_untyped', // unknown[]
            'unchecked mounts_untyped', // Handler | unknown collapses to unknown
            'unchecked mounts_untyped', // unconstrained type parameter
            'verified', // Handler | number: the instance fits the handler member
        ]);
    });
    it('judges a handle twin by the handler rule (K1a)', async () => {
        const twin = (id, member) => ({
            ...claim(id, member, 'instance:()', 'createApp'),
            claim: { kind: 'scope', member, name: { arg: 0 }, mounts: { arg: 1, handle: true }, of: 'instance:()' },
        });
        const response = await client.send({
            request_id: 'mounts-rest-handle',
            action: 'verify_library_claims',
            from_dir: root,
            checks: [
                {
                    claim_id: 'make',
                    package: 'fixture-rest',
                    export: 'createApp',
                    role: 'server_framework',
                    receiver: 'export',
                    claim: { kind: 'make', form: 'call', member: null },
                },
                claim('useHandlers', 'useHandlers', 'instance:()', 'createApp'),
                twin('useHandlers-handle', 'useHandlers'),
                twin('use-handle', 'use'),
            ],
        }, 60_000);
        assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
        const read = response.verdicts.map(v => (v.verdict === 'verified' ? 'verified' : `${v.verdict} ${v.reason}`));
        assert.deepStrictEqual(read.slice(1), [
            'failed mounts_not_instance', // unflagged: unchanged
            'verified', // its handle twin: the handlers are declared functions
            'verified', // Array<Handler | App>: every part is callable
        ]);
    });
});
