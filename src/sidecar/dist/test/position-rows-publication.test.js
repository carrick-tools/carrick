/**
 * A `call` row says how the declaration it names is published (carrick#2508):
 * {@link FLAG_PUBLISHED} when its module exports it under the row's symbol
 * name, or it is a global, and {@link FLAG_MODULE_DEFAULT} when it is its
 * module's `export =` or `default` target. A member of an interface or type
 * literal carries neither, whatever its name, and so does a declaration its
 * module publishes only under another name.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';
import { FLAG_MODULE_DEFAULT, FLAG_PUBLISHED } from '../src/position-rows.js';
const here = path.dirname(fileURLToPath(import.meta.url));
// Compiled to dist/test; the fixture lives beside the sources.
const FIXTURE = path.join(here, '..', '..', 'test', 'fixtures', 'position-rows-publication');
describe('position_rows: how a call row\'s declaration is published (carrick#2508)', () => {
    let root;
    let rows;
    let source;
    before(async () => {
        root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-publication-rows-')));
        const repo = path.join(root, 'repo');
        fs.mkdirSync(path.join(root, 'store'));
        fs.cpSync(FIXTURE, repo, { recursive: true });
        source = fs.readFileSync(path.join(repo, 'src', 'main.ts'), 'utf8');
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
                files: [path.join(repo, 'src', 'main.ts')],
                scan_root: repo,
                out_path: out,
                service: 'svc',
            }, 60_000);
            assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
            const lines = zlib.gunzipSync(fs.readFileSync(out)).toString('utf8').trimEnd().split('\n').slice(1);
            rows = JSON.parse(lines[0]).r;
        }
        finally {
            await client.stop();
        }
    });
    after(() => {
        fs.rmSync(root, { recursive: true, force: true });
    });
    /**
     * The innermost `call` row starting at `text`'s first occurrence (in
     * `makeApp().post(…)` the factory call, not the route call): its symbol
     * and its two publication bits.
     */
    const call = (text) => {
        const start = source.indexOf(text);
        assert.ok(start >= 0, text);
        const row = rows
            .filter((r) => r[0] === 'call' && r[1] === start)
            .sort((a, b) => a[2] - b[2])[0];
        assert.ok(row, `a call row at ${text}`);
        const bits = [row[8] & FLAG_PUBLISHED ? 'published' : '', row[8] & FLAG_MODULE_DEFAULT ? 'default' : '']
            .filter(Boolean)
            .join('+');
        return [row[3], bits];
    };
    it('marks module exports, globals and whole-module targets, never a member', () => {
        assert.deepStrictEqual([
            'res.json(',
            'json();',
            'renamed()',
            'http.get(',
            'parse()',
            'makeApp()',
            'make();',
            'make.json()',
            'start()',
            'fork()',
            'fetch(',
            'ambientGlobal()',
            'local()',
        ].map(call), [
            // A member of an interface, named as the package's export is.
            ['json', ''],
            ['json', 'published'],
            // Published only as `renamed`.
            ['inner', ''],
            ['get', ''],
            // Through a local barrel, under another name: still the export.
            ['json', 'published'],
            // The `export =` target, reached through a local barrel.
            ['make', 'default'],
            ['make', 'default'],
            // A member of the namespace the `export =` target merges with.
            ['json', 'published'],
            ['default', 'published+default'],
            ['fork', 'published'],
            ['fetch', 'published'],
            ['ambientGlobal', 'published'],
            // Not exported.
            ['local', ''],
        ]);
    });
});
