/**
 * A name bound by `require()` in a TypeScript file is typed `any` and has only
 * the local binding as its declaration. Its position rows name the export the
 * call resolves to instead (carrick#2454). JavaScript files are unchanged
 * (TypeScript resolves those itself), and so is every row the rule does not
 * cover.
 *
 * `fixtures/position-rows-require.golden.json` holds the rows the walk gave
 * before the rule, for the same fixture. Only the symbol, declaration file,
 * line and package columns may differ from it, and only at the rows below.
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
const FIXTURE = path.join(here, '..', '..', 'test', 'fixtures', 'position-rows-require');
const GOLDEN = path.join(here, '..', '..', 'test', 'fixtures', 'position-rows-require.golden.json');
/** The golden predates the publication bits (carrick#2508): compare without them. */
const withoutPublication = (row) => [...row.slice(0, 8), row[8] & ~(FLAG_PUBLISHED | FLAG_MODULE_DEFAULT)];
describe('position_rows: a name bound by require() in a TypeScript file (carrick#2454)', () => {
    let root;
    let files;
    let sources;
    const golden = JSON.parse(fs.readFileSync(GOLDEN, 'utf8'));
    before(async () => {
        root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-require-rows-')));
        const repo = path.join(root, 'repo');
        fs.mkdirSync(path.join(root, 'store'));
        fs.cpSync(FIXTURE, repo, { recursive: true });
        sources = {};
        const names = ['main.ts', 'negatives.ts', 'plain.js'];
        for (const n of names)
            sources[`src/${n}`] = fs.readFileSync(path.join(repo, 'src', n), 'utf8');
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
                files: names.map((n) => path.join(repo, 'src', n)),
                scan_root: repo,
                out_path: out,
                service: 'svc',
            }, 60_000);
            assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
            const lines = zlib
                .gunzipSync(fs.readFileSync(out))
                .toString('utf8')
                .trimEnd()
                .split('\n')
                .slice(1)
                .map((l) => JSON.parse(l));
            files = Object.fromEntries(lines.map((l) => [l.f, l.r.map(withoutPublication)]));
        }
        finally {
            await client.stop();
        }
    });
    after(() => {
        fs.rmSync(root, { recursive: true, force: true });
    });
    /** The row of `kind` whose text is `text`, at its `nth` occurrence in the file. */
    function rowFor(file, kind, text, nth = 0) {
        const source = sources[file];
        const matches = (files[file] ?? []).filter((r) => r[0] === kind && source.slice(r[1], r[2]) === text);
        assert.ok(matches[nth], `no ${kind} row for ${text} #${nth} in ${file}`);
        return matches[nth];
    }
    const place = (row) => `${row[3]} ${row[4]}:${row[5]} ${row[6]}`;
    it('names the export for a destructured name and a call through it', () => {
        assert.strictEqual(place(rowFor('src/main.ts', 'ident', 'fetchIt', 0)), 'fetchIt node_modules/dep/index.d.ts:1 dep');
        assert.strictEqual(place(rowFor('src/main.ts', 'call', 'fetchIt("x")')), 'fetchIt node_modules/dep/index.d.ts:1 dep');
    });
    it('names the property for a renamed name', () => {
        assert.strictEqual(place(rowFor('src/main.ts', 'call', 'renamed(1)')), 'other node_modules/dep/index.d.ts:2 dep');
    });
    it('names the module for a whole-module binding and the export for a member through it', () => {
        assert.strictEqual(place(rowFor('src/main.ts', 'ident', 'dep', 0)), 'node_modules/dep/index.d.ts node_modules/dep/index.d.ts:1 dep');
        assert.strictEqual(place(rowFor('src/main.ts', 'member', 'dep.fetchIt')), 'fetchIt node_modules/dep/index.d.ts:1 dep');
        assert.strictEqual(place(rowFor('src/main.ts', 'call', 'dep.fetchIt("y")')), 'fetchIt node_modules/dep/index.d.ts:1 dep');
    });
    it('names a repo module and its export', () => {
        assert.strictEqual(place(rowFor('src/main.ts', 'member', 'helpers.help')), 'help src/helpers.ts:1 svc');
        assert.strictEqual(place(rowFor('src/main.ts', 'call', 'helpers.help(2)')), 'help src/helpers.ts:1 svc');
    });
    it('names the export for require("m").name', () => {
        assert.strictEqual(place(rowFor('src/main.ts', 'ident', 'picked', 0)), 'limit node_modules/dep/index.d.ts:3 dep');
    });
    it('leaves the type text and flags as the checker reads them', () => {
        for (const [file, rows] of Object.entries(files)) {
            const before = golden[file];
            assert.strictEqual(rows.length, before.length, file);
            rows.forEach((row, i) => {
                assert.deepStrictEqual([row[0], row[1], row[2], row[7], row[8]], [before[i][0], before[i][1], before[i][2], before[i][7], before[i][8]]);
            });
        }
        assert.strictEqual(rowFor('src/main.ts', 'call', 'fetchIt("x")')[8] & 1, 1, 'still any');
    });
    it('changes nothing else: negatives, nested and default patterns, JavaScript', () => {
        const changed = new Set();
        for (const [file, rows] of Object.entries(files)) {
            rows.forEach((row, i) => {
                const before = golden[file][i];
                if (JSON.stringify(row) !== JSON.stringify(before))
                    changed.add(`${file}:${row[0]}:${sources[file].slice(row[1], row[2])}`);
            });
        }
        assert.deepStrictEqual([...changed].sort(), [
            'src/main.ts:call:dep.fetchIt("y")',
            'src/main.ts:call:fetchIt("x")',
            'src/main.ts:call:helpers.help(2)',
            'src/main.ts:call:renamed(1)',
            'src/main.ts:ident:dep',
            'src/main.ts:ident:fetchIt',
            'src/main.ts:ident:helpers',
            'src/main.ts:ident:picked',
            'src/main.ts:ident:renamed',
            'src/main.ts:member:dep.fetchIt',
            'src/main.ts:member:helpers.help',
        ].sort());
    });
});
