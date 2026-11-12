/**
 * A member whose symbol has no declaration names the type that owns it
 * (carrick#2448, slice A).
 *
 * A mapped type's members are made by the checker and carry no declaration.
 * The row of such a member names the mapped type node (or the alias that
 * names it) instead of leaving `decl_*` null. A plain object type is the
 * control: its member keeps its own property signature.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(__dirname, '..', '..', 'test', 'fixtures', 'position-rows-mapped-member');
describe('position_rows: a member with no declaration names its owner type (carrick#2448)', () => {
    let root;
    let rows;
    let source;
    before(async () => {
        root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-mapped-member-')));
        const repo = path.join(root, 'repo');
        fs.cpSync(path.join(FIXTURE, 'repo'), repo, { recursive: true });
        fs.cpSync(path.join(FIXTURE, 'installed'), path.join(repo, 'node_modules'), { recursive: true });
        const main = path.join(repo, 'src', 'main.ts');
        source = fs.readFileSync(main, 'utf8');
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
            const out = path.join(root, 'store', 'rows.jsonl.gz');
            const answer = await client.send({
                action: 'position_rows',
                request_id: 'rows',
                files: [main],
                scan_root: repo,
                out_path: out,
                service: 'svc',
            }, 60_000);
            assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
            const text = zlib.gunzipSync(fs.readFileSync(out)).toString('utf8');
            rows = text
                .trimEnd()
                .split('\n')
                .slice(1)
                .flatMap((line) => JSON.parse(line).r);
        }
        finally {
            await client.stop();
        }
    });
    after(() => {
        fs.rmSync(root, { recursive: true, force: true });
    });
    /** The row of a kind whose source text is exactly `text`. */
    const rowOf = (kind, text) => {
        const found = rows.filter((r) => r[0] === kind && source.slice(r[1], r[2]) === text);
        assert.strictEqual(found.length, 1, `rows for ${kind} ${text}: ${found.length}`);
        return found[0];
    };
    it('names the mapped type for a member read through a union receiver', () => {
        const row = rowOf('member', '(options.mapped ?? mapped).get');
        assert.deepStrictEqual([row[3], row[4], row[5], row[6]], ['get', 'src/client.ts', 3, 'svc']);
    });
    it('names the mapped type for the call of that member', () => {
        const row = rowOf('call', '(options.mapped ?? mapped).get("/a")');
        assert.deepStrictEqual([row[3], row[4], row[5]], ['get', 'src/client.ts', 3]);
    });
    it('names the mapped type for an element access', () => {
        const row = rowOf('member', 'mapped["post"]');
        assert.deepStrictEqual([row[4], row[5]], ['src/client.ts', 3]);
    });
    it('names the installed package for a mapped type in a declaration file', () => {
        const row = rowOf('member', 'depClient.post');
        assert.deepStrictEqual([row[3], row[4], row[5], row[6]], ['post', 'node_modules/dep/index.d.ts', 3, 'dep']);
    });
    it('leaves the plain object control on its own property signature', () => {
        const row = rowOf('member', '(options.plain ?? plain).get');
        assert.deepStrictEqual([row[3], row[4], row[5]], ['get', 'src/client.ts', 4]);
    });
});
