/**
 * carrick#2652: a scope's `mounts` slot whose parameter is typed by the
 * member's own type parameters (`child: Tree<SubEnv, SubBase>`). The receiver
 * is built at its defaults, so the parameter is read with the signature's own
 * type parameters erased to their constraints: the receiver fits when each of
 * its type arguments fits the constraint the parameter puts there.
 *
 * The package is invented, the general shape of a generic router class.
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
const TREE = `export type Handler = (request: { path: string }) => void;
export declare class Other<E extends object = {}> {
  readonly other: E;
}
export declare class Tree<Env extends object = {}, Base extends string = '/'> {
  readonly env: Env;
  readonly base: Base;
  get(path: string, handler: Handler): this;
  mount<SubEnv extends object, SubBase extends string>(prefix: string, child: Tree<SubEnv, SubBase>): this;
  mountNumeric<SubEnv extends number>(prefix: string, child: Tree<SubEnv, '/'>): this;
  mountOther<SubEnv extends object>(prefix: string, child: Other<SubEnv>): this;
  mountFixed<SubEnv extends object>(prefix: string, child: Tree<SubEnv, '/api'>): this;
}
`;
describe('verify_library_claims: mounts typed by the member own type parameters (carrick#2652)', () => {
    let root;
    let client;
    const check = (id, receiver, claim) => ({
        claim_id: id,
        package: 'fixture-tree',
        export: 'Tree',
        role: 'server_framework',
        receiver,
        claim,
    });
    const mounts = (member, id) => check(id, 'instance:new', { kind: 'scope', member, name: { arg: 0 }, mounts: { arg: 1 }, of: 'instance:new' });
    before(async () => {
        root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-claims-mounts-generic-'));
        const files = {
            'tsconfig.json': TSCONFIG,
            'package.json': JSON.stringify({ name: 'service' }),
            'src/index.ts': 'export const service = 1;\n',
            'node_modules/fixture-tree/package.json': JSON.stringify({ name: 'fixture-tree', version: '1.0.0', types: 'index.d.ts' }),
            'node_modules/fixture-tree/index.d.ts': TREE,
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
    it('erases the member type parameters to their constraints', async () => {
        const response = await client.send({
            request_id: 'mounts-generic',
            action: 'verify_library_claims',
            from_dir: root,
            checks: [
                check('make', 'export', { kind: 'make', form: 'new', member: null }),
                // Verified: the receiver's arguments fit the constraints.
                mounts('mount', 'generic'),
                // Not an instance: the argument the receiver holds does not fit the constraint.
                mounts('mountNumeric', 'numeric'),
                // Not an instance: another class.
                mounts('mountOther', 'other'),
                // Not an instance: the parameter fixes an argument the receiver's default is not.
                mounts('mountFixed', 'fixed'),
                // Not an instance: only a function fits the handler parameter.
                check('handler', 'instance:new', { kind: 'scope', member: 'get', name: { arg: 0 }, mounts: { arg: 1 }, of: 'instance:new' }),
            ],
        }, 60_000);
        assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
        const read = response.verdicts.map(v => (v.verdict === 'verified' ? 'verified' : `${v.verdict} ${v.reason}`));
        assert.deepStrictEqual(read.slice(1), [
            'verified',
            'failed mounts_not_instance',
            'failed mounts_not_instance',
            'failed mounts_not_instance',
            'failed mounts_not_instance',
        ]);
    });
});
