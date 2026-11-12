/**
 * Per-position compiler rows (carrick#2405).
 *
 * The `position_rows` action writes one row for each call, member access,
 * comparison and expression identifier of the files it is sent, as a gzip
 * JSON-lines file. The fixture is a two-package workspace
 * (`tests/fixtures/position-rows/`): a service under `apps/api` that imports a
 * workspace package by its `source` export condition, an installed package
 * inside the repo, one installed above it, and files that stress the
 * position key: multi-byte text, CRLF line endings, a BOM, an unresolved
 * import, plain JavaScript, JSX and a file the tsconfig does not include.
 *
 * The decompressed output is compared with the committed
 * `api.expected.jsonl`, exactly. The scanner reads the same file with its
 * serde types (`src/position_rows.rs`), so the file is the seam pin both
 * sides read.
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
const FIXTURE = path.join(__dirname, '..', '..', '..', '..', 'tests', 'fixtures', 'position-rows');
/**
 * The service's files as the scanner's walk sends them: every source file
 * under `apps/api`, sorted by repo-relative path, with the test file left out
 * by the walk (it is in the tree, and must not be read).
 */
const SERVICE_FILES = [
    'apps/api/orphan.ts',
    'apps/api/src/api.ts',
    'apps/api/src/bom.ts',
    'apps/api/src/broken.ts',
    'apps/api/src/crlf.ts',
    'apps/api/src/legacy.js',
    'apps/api/src/view.tsx',
];
/**
 * The fixture laid out in a temp dir: the repo, the workspace link a package
 * manager writes for `@fixture/shared`, and a package installed above the
 * repo root.
 */
function materialise() {
    const root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-position-rows-')));
    const repo = path.join(root, 'repo');
    fs.cpSync(path.join(FIXTURE, 'repo'), repo, { recursive: true });
    fs.cpSync(path.join(FIXTURE, 'hoisted'), path.join(root, 'node_modules'), { recursive: true });
    fs.symlinkSync('../../packages/shared', path.join(repo, 'node_modules', '@fixture', 'shared'));
    return { root, repo, store: path.join(root, 'store') };
}
/** One walk of the service in a fresh sidecar, and the decompressed rows file. */
async function walk(workspace, outName) {
    const client = new SidecarClient();
    await client.start();
    try {
        const ready = await client.send({
            action: 'init',
            request_id: 'init',
            repo_root: path.join(workspace.repo, 'apps', 'api'),
            tsconfig_path: 'tsconfig.json',
            scan_root: workspace.repo,
        });
        assert.strictEqual(ready.status, 'ready');
        const outPath = path.join(workspace.store, 'positions', outName);
        const answer = await client.send({
            action: 'position_rows',
            request_id: 'rows',
            files: SERVICE_FILES.map((file) => path.join(workspace.repo, file)),
            scan_root: workspace.repo,
            out_path: outPath,
            service: 'api',
        }, 60_000);
        assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
        assert.ok(!fs.existsSync(`${outPath}.partial`), 'the temporary file is renamed into place');
        const gz = fs.readFileSync(outPath);
        return { answer, text: zlib.gunzipSync(gz).toString('utf8'), gz };
    }
    finally {
        await client.stop();
    }
}
describe('position_rows (carrick#2405)', () => {
    let workspace;
    let first;
    let second;
    before(async () => {
        workspace = materialise();
        first = await walk(workspace, 'repo__api.jsonl.gz');
        second = await walk(workspace, 'again.jsonl.gz');
    });
    after(() => {
        fs.rmSync(workspace.root, { recursive: true, force: true });
    });
    it('writes the committed rows, exactly', () => {
        const expected = fs.readFileSync(path.join(FIXTURE, 'api.expected.jsonl'), 'utf8');
        assert.strictEqual(first.text, expected);
    });
    // `api.v1-kinds.jsonl` is what the version-1 walk wrote (carrick#2430).
    it('leaves every row of a version-1 kind byte-identical', () => {
        const expected = fs.readFileSync(path.join(FIXTURE, 'api.v1-kinds.jsonl'), 'utf8');
        assert.strictEqual(asVersion1Rows(first.text), expected);
    });
    it('writes the same bytes on a second walk', () => {
        assert.ok(first.gz.equals(second.gz), 'two walks of one tree wrote different bytes');
    });
    it('counts what it wrote', () => {
        const lines = first.text.trimEnd().split('\n').slice(1).map((line) => JSON.parse(line));
        const rows = lines.flatMap((line) => line.r);
        assert.strictEqual(first.answer.result?.files, SERVICE_FILES.length);
        assert.strictEqual(first.answer.result?.rows, rows.length);
        assert.strictEqual(first.answer.result?.resolved, rows.filter((row) => row[3] !== null).length);
        assert.strictEqual(first.answer.result?.bytes, first.gz.length);
    });
    it('writes no absolute path', () => {
        assert.ok(!first.text.includes(workspace.root), 'a row holds the temp dir');
        assert.ok(!first.text.includes(os.homedir()), 'a row holds the home dir');
        assert.ok(!/"\/[^"]/.test(first.text), 'a value starts with /');
    });
    it('walks a file the tsconfig does not include', () => {
        const orphan = first.text
            .split('\n')
            .map((line) => (line ? JSON.parse(line) : undefined))
            .find((line) => line?.f === 'apps/api/orphan.ts');
        assert.ok(orphan, 'orphan.ts has a line');
        assert.strictEqual(orphan.skip, undefined);
        assert.ok(orphan.r.length > 0, 'orphan.ts has rows');
    });
    it('reads only the files it was sent', () => {
        assert.ok(!first.text.includes('__tests__'), 'the test file was read');
        const header = JSON.parse(first.text.split('\n')[0]);
        assert.strictEqual(header.files, SERVICE_FILES.length);
    });
    it('refuses to write inside the scanned repo outside .carrick', async () => {
        const client = new SidecarClient();
        await client.start();
        try {
            await client.send({
                action: 'init',
                request_id: 'init',
                repo_root: path.join(workspace.repo, 'apps', 'api'),
                tsconfig_path: 'tsconfig.json',
                scan_root: workspace.repo,
            });
            const outPath = path.join(workspace.repo, 'positions', 'rows.jsonl.gz');
            const answer = await client.send({
                action: 'position_rows',
                request_id: 'rows',
                files: SERVICE_FILES.map((file) => path.join(workspace.repo, file)),
                scan_root: workspace.repo,
                out_path: outPath,
                service: 'api',
            }, 60_000);
            assert.strictEqual(answer.status, 'error');
            assert.ok(answer.errors?.[0]?.includes('carrick#1748'), JSON.stringify(answer.errors));
            assert.ok(!fs.existsSync(path.join(workspace.repo, 'positions')), 'the repo was written');
        }
        finally {
            await client.stop();
        }
    });
});
