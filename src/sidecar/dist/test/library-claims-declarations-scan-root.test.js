/**
 * carrick#2474 R1 part 1, follow-up: a verified `op` verdict's `declarations`
 * are spelled from the scan root, as position rows spell `decl_file`, so the
 * two join for a service that is not at the repo root. The fixture is a repo
 * with a service under `services/web`: one package installed under the
 * service, one hoisted to the repo root. Each verdict is compared with the
 * row the `position_rows` walk writes for the same call.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import { SidecarClient } from './helpers.js';
const app = (member) => `export type Handler = (request: unknown, response: unknown) => void;
export interface App {
  ${member}(path: string, handler: Handler): this;
}
export default function make(): App;
`;
const SOURCE = `import makeLocal from 'local-server';
import makeHoisted from 'hoisted-server';
makeLocal().get('/a', () => {});
makeHoisted().post('/b', () => {});
`;
describe('verify_library_claims: declarations spelled from the scan root (carrick#2474)', () => {
    let tmp;
    let repo;
    let service;
    let rows;
    const init = (client) => client.send({
        action: 'init',
        request_id: 'init',
        repo_root: service,
        tsconfig_path: 'tsconfig.json',
        scan_root: repo,
    });
    before(async () => {
        tmp = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-claims-scan-root-')));
        repo = path.join(tmp, 'repo');
        service = path.join(repo, 'services', 'web');
        const write = (rel, text) => {
            const target = path.join(repo, rel);
            fs.mkdirSync(path.dirname(target), { recursive: true });
            fs.writeFileSync(target, text);
        };
        write('package.json', JSON.stringify({ name: 'repo', private: true }));
        write('node_modules/hoisted-server/package.json', JSON.stringify({ name: 'hoisted-server', version: '1.0.0', types: 'index.d.ts' }));
        write('node_modules/hoisted-server/index.d.ts', app('post'));
        write('services/web/package.json', JSON.stringify({ name: 'web' }));
        write('services/web/tsconfig.json', JSON.stringify({
            compilerOptions: { target: 'es2020', module: 'commonjs', moduleResolution: 'node', strict: true, skipLibCheck: true, types: [] },
            include: ['src/**/*.ts'],
        }));
        write('services/web/node_modules/local-server/package.json', JSON.stringify({ name: 'local-server', version: '1.0.0', types: 'index.d.ts' }));
        write('services/web/node_modules/local-server/index.d.ts', app('get'));
        write('services/web/src/index.ts', SOURCE);
        // The rows, from a process of their own, as the scanner asks for them.
        const walker = new SidecarClient();
        await walker.start();
        try {
            assert.strictEqual((await init(walker)).status, 'ready');
            const outPath = path.join(tmp, 'store', 'web.jsonl.gz');
            const answer = await walker.send({
                action: 'position_rows',
                request_id: 'rows',
                files: [path.join(service, 'src', 'index.ts')],
                scan_root: repo,
                out_path: outPath,
                service: 'web',
            }, 60_000);
            assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
            const text = zlib.gunzipSync(fs.readFileSync(outPath)).toString('utf8');
            rows = text
                .trimEnd()
                .split('\n')
                .slice(1)
                .flatMap((line) => JSON.parse(line).r);
        }
        finally {
            await walker.stop();
        }
    });
    after(() => {
        fs.rmSync(tmp, { recursive: true, force: true });
    });
    /** The `decl_file` and `decl_line` of the call row whose symbol is `member`. */
    const rowOf = (member) => {
        const found = rows.filter((row) => row[0] === 'call' && row[3] === member);
        assert.strictEqual(found.length, 1, `one call row for ${member}: ${JSON.stringify(found)}`);
        return { decl_file: found[0][4], decl_line: found[0][5] };
    };
    it('a package under the service and one hoisted to the repo root name the declaration the rows name', async () => {
        const client = new SidecarClient();
        await client.start();
        try {
            assert.strictEqual((await init(client)).status, 'ready');
            const check = (id, pkg, receiver, claim) => ({
                claim_id: id,
                package: pkg,
                export: 'default',
                role: 'server_framework',
                receiver,
                claim,
            });
            const route = (member) => ({ kind: 'op', op: 'receive', member, name: { arg: 0 }, of: 'instance:()' });
            const maker = { kind: 'make', form: 'call', member: null };
            const response = await client.send({
                request_id: 'claims',
                action: 'verify_library_claims',
                from_dir: service,
                scan_root: repo,
                checks: [
                    check('l0', 'local-server', 'export', maker),
                    check('l1', 'local-server', 'instance:()', route('get')),
                    check('h0', 'hoisted-server', 'export', maker),
                    check('h1', 'hoisted-server', 'instance:()', route('post')),
                ],
            }, 60_000);
            assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
            const [, local, , hoisted] = response.verdicts;
            assert.strictEqual(local.verdict, 'verified', JSON.stringify(local));
            assert.strictEqual(hoisted.verdict, 'verified', JSON.stringify(hoisted));
            const localRow = rowOf('get');
            const hoistedRow = rowOf('post');
            // The rows themselves are spelled from the repo root.
            assert.strictEqual(localRow.decl_file, 'services/web/node_modules/local-server/index.d.ts');
            assert.strictEqual(hoistedRow.decl_file, 'node_modules/hoisted-server/index.d.ts');
            assert.deepStrictEqual(local.declarations, [localRow]);
            assert.deepStrictEqual(hoisted.declarations, [hoistedRow]);
        }
        finally {
            await client.stop();
        }
    });
});
