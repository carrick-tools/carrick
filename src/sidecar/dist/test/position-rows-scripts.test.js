/**
 * Position rows for the JavaScript files no owner program takes (carrick#2413).
 *
 * The fixture (`tests/fixtures/position-rows-scripts/`) is a service whose
 * tsconfig has no `allowJs`. Its `.mjs` and `.cjs` files are in no program the
 * service's config builds, so the walk reads them in that config's `scripts`
 * program: the same options with JavaScript accepted. A name a script imports
 * from a TypeScript module or a sibling script, or reads from a global the
 * service's own declaration file declares, is attributed to its declaration.
 * What the owner's program reads is unchanged, and the walk writes nothing but
 * its rows file.
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
const FIXTURE = path.join(__dirname, '..', '..', '..', '..', 'tests', 'fixtures', 'position-rows-scripts');
const TYPESCRIPT_FILES = ['apps/web/src/config.ts', 'apps/web/src/env.d.ts', 'apps/web/src/main.ts'];
const SCRIPT_FILES = ['apps/web/src/helper.mjs', 'apps/web/src/legacy.cjs', 'apps/web/src/routes.mjs'];
/** One walk of `files` in a fresh sidecar; the file lines by path. */
async function walk(repo, outPath, files) {
    const client = new SidecarClient();
    await client.start();
    try {
        const ready = await client.send({
            action: 'init',
            request_id: 'init',
            repo_root: path.join(repo, 'apps', 'web'),
            tsconfig_path: 'tsconfig.json',
            scan_root: repo,
        });
        assert.strictEqual(ready.status, 'ready');
        const answer = await client.send({
            action: 'position_rows',
            request_id: 'rows',
            files: files.map((file) => path.join(repo, file)),
            scan_root: repo,
            out_path: outPath,
            service: 'web',
        }, 60_000);
        assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
        assert.strictEqual(answer.result?.files, files.length);
        const text = zlib.gunzipSync(fs.readFileSync(outPath)).toString('utf8');
        const lines = text.trimEnd().split('\n').slice(1).map((line) => JSON.parse(line));
        assert.deepStrictEqual(lines.map((line) => line.f), files, 'one line per file, in request order');
        return new Map(lines.map((line) => [line.f, line]));
    }
    finally {
        await client.stop();
    }
}
/** Every file under `dir` with its size and modification time, `.git` aside. */
function treeState(dir) {
    const out = [];
    const visit = (at) => {
        for (const entry of fs.readdirSync(at, { withFileTypes: true })) {
            const full = path.join(at, entry.name);
            const stat = fs.lstatSync(full);
            out.push(`${path.relative(dir, full)} ${entry.isDirectory() ? 'dir' : `${stat.size} ${stat.mtimeMs}`}`);
            if (entry.isDirectory())
                visit(full);
        }
    };
    visit(dir);
    return out.sort();
}
/** The row of `kind` whose text in `source` is `text`. */
function rowAt(line, source, kind, text) {
    const row = line.r.find((r) => r[0] === kind && source.slice(r[1], r[2]) === text);
    assert.ok(row, `${line.f}: no ${kind} row for ${text}`);
    return row;
}
describe('position_rows: scripts no owner program takes (carrick#2413)', () => {
    let root;
    let repo;
    let treeBefore;
    let all;
    let typescriptOnly;
    before(async () => {
        root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-position-scripts-')));
        repo = path.join(root, 'repo');
        fs.cpSync(path.join(FIXTURE, 'repo'), repo, { recursive: true });
        treeBefore = treeState(repo);
        const files = [...TYPESCRIPT_FILES, ...SCRIPT_FILES].sort();
        all = await walk(repo, path.join(root, 'store', 'all.jsonl.gz'), files);
        typescriptOnly = await walk(repo, path.join(root, 'store', 'ts.jsonl.gz'), TYPESCRIPT_FILES);
    });
    after(() => {
        fs.rmSync(root, { recursive: true, force: true });
    });
    const source = (file) => fs.readFileSync(path.join(repo, file), 'utf8');
    it('walks every script the service config does not take', () => {
        for (const file of SCRIPT_FILES) {
            const line = all.get(file);
            assert.strictEqual(line.skip, undefined, `${file} was skipped`);
            assert.ok(line.r.length > 0, `${file} has no rows`);
            assert.match(line.h ?? '', /^[0-9a-f]{64}$/);
        }
    });
    it('attributes what a script imports or reads to its declaration', () => {
        const file = 'apps/web/src/routes.mjs';
        const text = source(file);
        const line = all.get(file);
        const base = rowAt(line, text, 'ident', 'BASE_PATH');
        assert.deepStrictEqual([base[3], base[4], base[5]], ['BASE_PATH', 'apps/web/src/config.ts', 1]);
        const helper = rowAt(line, text, 'call', 'helper(route)');
        assert.deepStrictEqual([helper[3], helper[4], helper[5]], ['helper', 'apps/web/src/helper.mjs', 1]);
        // A global the service's own declaration file declares.
        const build = rowAt(line, text, 'ident', 'BUILD_ID');
        assert.deepStrictEqual([build[3], build[4], build[7]], ['BUILD_ID', 'apps/web/src/env.d.ts', 'string']);
        const list = rowAt(line, text, 'call', 'new Client().list()');
        assert.deepStrictEqual([list[3], list[4], list[7]], ['list', 'apps/web/src/config.ts', 'string[]']);
    });
    it('follows a CommonJS require from a .cjs file', () => {
        const file = 'apps/web/src/legacy.cjs';
        const line = all.get(file);
        const base = rowAt(line, source(file), 'ident', 'BASE_PATH');
        assert.deepStrictEqual([base[4], base[7]], ['apps/web/src/config.ts', '"/api"']);
    });
    it('leaves what the owner program reads unchanged', () => {
        for (const file of TYPESCRIPT_FILES) {
            assert.deepStrictEqual(all.get(file), typescriptOnly.get(file), file);
        }
    });
    it('writes nothing but its rows file', () => {
        assert.deepStrictEqual(treeState(repo), treeBefore, 'the walk changed the scanned tree');
        assert.deepStrictEqual(fs.readdirSync(root).sort(), ['repo', 'store']);
        assert.deepStrictEqual(fs.readdirSync(path.join(root, 'store')).sort(), ['all.jsonl.gz', 'ts.jsonl.gz']);
    });
});
