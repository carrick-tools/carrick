/**
 * A row whose name resolves to a module (a namespace import, a namespace
 * re-export used as a value) names it the way `decl_file` names a file, never
 * by the compiler's quoted absolute file name (carrick#2419).
 *
 * The fixture is written here: a service that imports an installed package as
 * a namespace and a repo module through a namespace re-export. Two copies at
 * different paths must give byte-identical rows.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import { SidecarClient } from './helpers.js';
const FILES = {
    'package.json': '{"name":"svc","version":"1.0.0"}',
    'tsconfig.json': '{"compilerOptions":{"strict":true,"module":"commonjs","target":"es2020","moduleResolution":"node"},"include":["src"]}',
    'node_modules/dep/package.json': '{"name":"dep","version":"1.0.0","types":"index.d.ts"}',
    'node_modules/dep/index.d.ts': 'export declare function fetchIt(url: string): Promise<string>;\n',
    'src/mod.ts': 'export function inside(): number { return 1; }\n',
    'src/barrel.ts': 'export * as modNs from "./mod";\n',
    'src/main.ts': [
        'import * as dep from "dep";',
        'import { modNs } from "./barrel";',
        'export const a = dep.fetchIt("x");',
        'export const b = modNs.inside();',
        'export const c = [dep, modNs];',
        '',
    ].join('\n'),
};
async function rowsOf(repo, out) {
    const client = new SidecarClient();
    await client.start();
    try {
        const ready = await client.send({
            action: 'init',
            request_id: 'init',
            repo_root: repo,
            tsconfig_path: 'tsconfig.json',
            scan_root: repo,
        });
        assert.strictEqual(ready.status, 'ready');
        const answer = await client.send({
            action: 'position_rows',
            request_id: 'rows',
            files: [path.join(repo, 'src', 'main.ts')],
            scan_root: repo,
            out_path: out,
            service: 'svc',
        }, 60_000);
        assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
        const text = zlib.gunzipSync(fs.readFileSync(out)).toString('utf8');
        const lines = text.trimEnd().split('\n').slice(1).map((l) => JSON.parse(l));
        return { text: lines.map((l) => JSON.stringify(l)).join('\n'), rows: lines.flatMap((l) => l.r) };
    }
    finally {
        await client.stop();
    }
}
describe('position_rows: module symbols carry no absolute path (carrick#2419)', () => {
    let root;
    let one;
    let two;
    before(async () => {
        root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-module-symbols-')));
        fs.mkdirSync(path.join(root, 'store'));
        const make = async (name) => {
            const repo = path.join(root, 'trees', name, 'deeper', 'checkout');
            for (const [file, body] of Object.entries(FILES)) {
                fs.mkdirSync(path.dirname(path.join(repo, file)), { recursive: true });
                fs.writeFileSync(path.join(repo, file), body);
            }
            return rowsOf(repo, path.join(root, 'store', `${name}.jsonl.gz`));
        };
        one = await make('a');
        two = await make('b-longer-name');
    });
    after(() => {
        fs.rmSync(root, { recursive: true, force: true });
    });
    it('names a namespace import and a namespace re-export like decl_file', () => {
        const symbols = one.rows.map((r) => r[3]).filter((s) => s !== null);
        assert.ok(symbols.length > 0);
        for (const symbol of symbols) {
            assert.ok(!/^["']?(\/|[A-Za-z]:[\\/])/.test(symbol), `absolute symbol: ${symbol}`);
        }
        const idents = one.rows.filter((r) => r[0] === 'ident' && (r[4] ?? '').length > 0);
        assert.ok(idents.some((r) => r[3] === 'node_modules/dep/index.d.ts' && r[4] === r[3]), 'the installed package namespace');
        assert.ok(idents.some((r) => r[3] === 'src/mod.ts' && r[4] === r[3]), 'the repo module through the re-export');
    });
    it('writes byte-identical rows from two checkout paths', () => {
        assert.strictEqual(one.text, two.text);
    });
});
