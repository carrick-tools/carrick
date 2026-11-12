/**
 * `jsx` rows (carrick#2430) record each JSX element's tag. Version 4
 * (carrick#2561) writes no `impl` rows: the walk runs no go-to-implementation
 * search, so the bodyless declarations below get none.
 *
 * The fixture (`test/fixtures/position-rows-impl/`) is an interface `Port`
 * implemented by a class and by an object literal, an abstract class with two
 * subclasses, a function-typed constructor parameter, an interface with more
 * implementations than the cap, and two `.tsx` files with an imported
 * component and an intrinsic element. `v1-kinds.expected.jsonl` is what the
 * version-1 walk wrote for it: the rows of the version-1 kinds must not move.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { SidecarClient, asVersion1Rows } from './helpers.js';
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(__dirname, '..', '..', 'test', 'fixtures', 'position-rows-impl');
/** Walk every source file of a copy of the fixture at `repo`, in path order. */
async function walk(repo, out) {
    const files = fs
        .readdirSync(path.join(repo, 'src'))
        .filter((file) => !file.endsWith('.d.ts'))
        .sort()
        .map((file) => path.join(repo, 'src', file));
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
        const answer = await client.send({ action: 'position_rows', request_id: 'rows', files, scan_root: repo, out_path: out, service: 'svc' }, 60_000);
        assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
        const text = zlib.gunzipSync(fs.readFileSync(out)).toString('utf8');
        const rows = new Map();
        const sources = new Map();
        for (const line of text.trimEnd().split('\n').slice(1)) {
            const file = JSON.parse(line);
            rows.set(file.f, file.r);
            sources.set(file.f, fs.readFileSync(path.join(repo, file.f), 'utf8'));
        }
        return { text, rows, sources, counts: answer.result ?? {} };
    }
    finally {
        await client.stop();
    }
}
describe('position_rows version 4: jsx rows, no impl rows (carrick#2430, #2561)', () => {
    let root;
    let one;
    let two;
    before(async () => {
        root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-rows-impl-')));
        fs.mkdirSync(path.join(root, 'store'));
        const make = (name) => {
            const repo = path.join(root, 'trees', name, 'checkout');
            fs.cpSync(path.join(FIXTURE, 'repo'), repo, { recursive: true });
            return walk(repo, path.join(root, 'store', `${name}.jsonl.gz`));
        };
        one = await make('a');
        two = await make('b-longer-name');
    });
    after(() => {
        fs.rmSync(root, { recursive: true, force: true });
    });
    /** The rows of `kind` in `file` whose source text is exactly `text`. */
    const rowsAt = (file, kind, text) => {
        const source = one.sources.get(file) ?? '';
        return (one.rows.get(file) ?? []).filter((r) => r[0] === kind && source.slice(r[1], r[2]) === text);
    };
    it('writes only the version-4 kinds, with no impl row at an interface, abstract or capped call', () => {
        const kinds = new Set([...one.rows.values()].flat().map((r) => r[0]));
        assert.deepStrictEqual([...kinds].sort(), ['call', 'ident', 'jsx', 'member']);
        assert.ok(rowsAt('src/main.ts', 'call', "port.send('created', { id: 1 })").length === 1);
        assert.ok(rowsAt('src/main.ts', 'call', 'step.run()').length === 1);
    });
    it('writes a jsx row for each element: the imported component and the intrinsic', () => {
        const tag = (file, text) => rowsAt(file, 'jsx', text).map((r) => [r[3], r[4], r[5], r[6]]);
        assert.deepStrictEqual(tag('src/view.tsx', 'Badge'), [['Badge', 'src/badge.tsx', 1, 'svc']]);
        assert.deepStrictEqual(tag('src/view.tsx', 'section'), [['section', 'src/jsx.d.ts', 6, 'svc']]);
        assert.deepStrictEqual(tag('src/badge.tsx', 'section'), [['section', 'src/jsx.d.ts', 6, 'svc']]);
        // An intrinsic reads as the attributes its tag is given, never the error type.
        assert.deepStrictEqual(rowsAt('src/view.tsx', 'jsx', 'section').map((r) => [r[7], r[8]]), [['{ title?: string | undefined; }', 0]]);
        const all = [...one.rows.values()].flat().filter((r) => r[0] === 'jsx');
        assert.strictEqual(all.length, 3, 'one jsx row per opening or self-closing element');
    });
    it('counts the jsx rows it wrote, and no impl count', () => {
        const all = [...one.rows.values()].flat();
        assert.ok(!('impl' in one.counts), JSON.stringify(one.counts));
        assert.strictEqual(one.counts.jsx, all.filter((r) => r[0] === 'jsx').length);
        assert.strictEqual(one.counts.rows, all.length);
    });
    it('leaves every row of a version-1 kind byte-identical', () => {
        const expected = fs.readFileSync(path.join(FIXTURE, 'v1-kinds.expected.jsonl'), 'utf8');
        assert.strictEqual(asVersion1Rows(one.text), expected);
    });
    it('writes byte-identical rows from two checkout paths', () => {
        assert.strictEqual(one.text, two.text);
    });
});
