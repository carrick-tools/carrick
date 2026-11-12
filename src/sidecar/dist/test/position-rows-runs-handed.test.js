/**
 * A `call` row carries {@link FLAG_RUNS_HANDED} when the call's declared
 * return is the result of the function it is handed (carrick#2634): the
 * library runs the function and returns what it returns. A function it keeps,
 * registers, times or returns itself does not. The bit is also pinned to the
 * Rust constant of the same name.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';
import { FLAG_RUNS_HANDED } from '../src/position-rows.js';
const here = path.dirname(fileURLToPath(import.meta.url));
// Compiled to dist/test; the fixture lives beside the sources.
const FIXTURE = path.join(here, '..', '..', 'test', 'fixtures', 'position-rows-runs-handed');
const RUST_ROWS = path.join(here, '..', '..', '..', 'position_rows.rs');
describe('position_rows: a call that runs the function it is handed (carrick#2634)', () => {
    let root;
    let rows;
    let source;
    before(async () => {
        root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-runs-handed-rows-')));
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
    /** Whether the outermost `call` row of the statement starting `text` has the bit. */
    const runs = (text) => {
        const start = source.indexOf(text);
        assert.ok(start >= 0, text);
        const row = rows
            .filter((r) => r[0] === 'call' && r[1] === start)
            .sort((a, b) => b[2] - a[2])[0];
        assert.ok(row, `a call row at ${text}`);
        return (row[8] & FLAG_RUNS_HANDED) !== 0;
    };
    const WANTED = [
        ['within(ctx, () =>', true],
        ['inSpan("load"', true],
        ['attempt(async', true],
        ['measured(function', true],
        ['Promise.resolve(1).then(() => fetch', true],
        ['Promise.resolve(1).then(() => 1, () => 2)', true],
        // An array is not an awaitable.
        ['[1, 2].map(', false],
        // Kept for later.
        ['keep(', false],
        // Inside another generic type.
        ['store(', false],
        // Registered, or returns something else.
        ['on("ready"', false],
        ['every(1000', false],
        // Returns the function itself, not its result.
        ['wrap(', false],
        // One of the two functions is not run.
        ['both(', false],
        // `Parameters<F>` is not the result of `F`.
        ['argsOf(', false],
        // The executor's result is not returned.
        ['new Promise<void>', false],
        // A fact about the call, however the function is written.
        ['within(ctx, run)', true],
    ];
    it('is set on exactly the calls whose declared return is the handed result', () => {
        assert.deepStrictEqual(WANTED.map(([text]) => [text, runs(text)]), WANTED);
    });
    it('is set on no other row', () => {
        const wantedStarts = new Set(WANTED.filter(([, set]) => set).map(([text]) => source.indexOf(text)));
        const stray = rows.filter((r) => r[8] & FLAG_RUNS_HANDED && !(r[0] === 'call' && wantedStarts.has(r[1])));
        assert.deepStrictEqual(stray, []);
    });
    it('pins the bit to the Rust constant', () => {
        const rust = fs.readFileSync(RUST_ROWS, 'utf8');
        const match = /pub const FLAG_RUNS_HANDED: u16 = (\d+);/.exec(rust);
        assert.ok(match, 'src/position_rows.rs declares FLAG_RUNS_HANDED');
        assert.strictEqual(Number(match[1]), FLAG_RUNS_HANDED);
    });
});
