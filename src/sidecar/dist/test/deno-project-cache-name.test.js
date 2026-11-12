/**
 * The Deno cache folder's name comes from where the service sits in its
 * workspace, never from the checkout's absolute path. The name flows into the
 * relocated declarations, so a name that follows the path made `surface_hash`
 * and the type-reach graph differ for the same tree checked out twice.
 */
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { spawnSync } from 'node:child_process';
import { captureStub, DenoProject, findDenoConfig } from '../src/capture/index.js';
import { SidecarClient } from './helpers.js';
const hasDeno = spawnSync('deno', ['--version']).status === 0;
const MAIN = (id) => `export interface Row { ${id}: string }\nexport function read(): Row { return { ${id}: "a" }; }\n`;
/** The same Deno tree under a fresh absolute path; `name` varies its length. */
function checkout(name, field = 'id') {
    const base = fs.mkdtempSync(path.join(os.tmpdir(), `carrick-deno-name-${name}-`));
    const root = path.join(base, 'repo');
    fs.mkdirSync(root);
    fs.writeFileSync(path.join(root, 'deno.json'), JSON.stringify({}));
    fs.writeFileSync(path.join(root, 'main.ts'), MAIN(field));
    return root;
}
function surfaceHash(root) {
    const captured = captureStub({
        repoRoot: root,
        serviceName: 'svc',
        outDir: path.join(root, '.carrick', 'stub'),
        anchors: [{ kind: 'symbol', alias: 'Row', source_file: 'main.ts', symbol_name: 'Row', anchor_origin: 'llm-symbol' }],
    });
    assert.equal(captured.success, true, captured.errors.join('\n'));
    const hash = captured.aliases[0].surface_hash;
    assert.match(String(hash), /^[0-9a-f]{64}$/);
    return hash;
}
async function reachGraph(root) {
    const client = new SidecarClient();
    await client.start();
    try {
        const init = await client.send({ action: 'init', request_id: 'init', repo_root: root, scan_root: root }, 60000);
        assert.equal(init.status, 'ready');
        const answer = await client.send({ action: 'reach_graph', request_id: 'graph' }, 60000);
        assert.equal(answer.status, 'success');
        return JSON.stringify(answer.graph);
    }
    finally {
        await client.stop();
    }
}
describe('Deno cache folder name', { skip: !hasDeno }, () => {
    it('is the same for one tree at two absolute paths, and differs between members', () => {
        const [a, b] = [checkout('a'), checkout('longer-name-b')];
        const folder = (root) => path.relative(root, new DenoProject(findDenoConfig(root), root).cacheDir);
        assert.equal(folder(a), folder(b));
        const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-deno-members-'));
        for (const member of ['one', 'two']) {
            fs.mkdirSync(path.join(workspace, member));
            fs.writeFileSync(path.join(workspace, member, 'deno.json'), '{}');
        }
        fs.writeFileSync(path.join(workspace, 'deno.json'), JSON.stringify({ workspace: ['./one', './two'] }));
        const cache = (member) => new DenoProject(findDenoConfig(path.join(workspace, member)), path.join(workspace, member)).cacheDir;
        assert.notEqual(cache('one'), cache('two'));
    });
    it('gives equal surface hashes and reach graphs at two paths; a changed declaration moves the hash', async () => {
        const [a, b, changed] = [checkout('a'), checkout('longer-name-b'), checkout('c', 'name')];
        assert.equal(surfaceHash(a), surfaceHash(b));
        assert.notEqual(surfaceHash(a), surfaceHash(changed));
        const graph = await reachGraph(a);
        assert.equal(graph, await reachGraph(b));
    });
});
