/**
 * WP3 seam additions to the v2 capture bundle:
 *  - `array_depth` on symbol anchors (#248/#306): the anchor is the ELEMENT
 *    symbol, so the use-site's `[]` levels ride the anchor and the surface
 *    alias becomes `import('./m').Sym[]`.
 *  - `literal` anchors (the v1 inline-alias path): verbatim type text, with
 *    bare identifiers resolved through a sibling symbol anchor's module when
 *    one names the same symbol (so they don't dangle in the entry file).
 *
 * Drives `captureStub` in-process against a synthetic mini-repo.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
let repoDir;
let outRoot;
function writeRepo() {
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
        compilerOptions: {
            strict: true,
            rootDir: 'src',
            module: 'esnext',
            moduleResolution: 'bundler',
            target: 'es2022',
            skipLibCheck: true,
        },
        include: ['src'],
    }));
    fs.writeFileSync(path.join(repoDir, 'src', 'types.ts'), 'export interface Widget { id: string; price: number; }\n');
}
describe('capture v2 WP3 anchors: array_depth + literal', () => {
    let result;
    let surface;
    before(() => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-wp3-repo-'));
        outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-wp3-stub-'));
        writeRepo();
        result = captureStub({
            repoRoot: repoDir,
            serviceName: 'wp3-svc',
            outDir: path.join(outRoot, 'stub'),
            anchors: [
                {
                    kind: 'symbol',
                    alias: 'Arr_Response',
                    symbol_name: 'Widget',
                    source_file: 'src/types.ts',
                    anchor_origin: 'llm-symbol',
                    array_depth: 2,
                },
                {
                    kind: 'symbol',
                    alias: 'Plain_Response',
                    symbol_name: 'Widget',
                    source_file: 'src/types.ts',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'literal',
                    alias: 'Lit_Sibling',
                    type_text: 'Widget',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'literal',
                    alias: 'Lit_Object',
                    type_text: '{ ok: boolean; count: number }',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'literal',
                    alias: 'Lit_Dangling',
                    type_text: 'NoSuchSymbolAnywhere',
                    anchor_origin: 'llm-symbol',
                },
            ],
        });
        assert.strictEqual(result.success, true, JSON.stringify(result.errors));
        surface = fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
    });
    after(() => {
        fs.rmSync(repoDir, { recursive: true, force: true });
        fs.rmSync(outRoot, { recursive: true, force: true });
    });
    function record(alias) {
        const r = result.aliases.find((a) => a.alias === alias);
        assert.ok(r, `no alias record for ${alias}`);
        return r;
    }
    it('wraps a symbol anchor in array_depth [] levels', () => {
        assert.match(surface, /Arr_Response = .*Widget\[\]\[\];/, `surface should carry Widget[][]:\n${surface}`);
        assert.strictEqual(record('Arr_Response').self_check, 'ok');
        assert.strictEqual(record('Arr_Response').serialization, 'emitted');
    });
    it('captures a depth-less symbol anchor unwrapped', () => {
        assert.match(surface, /Plain_Response = .*Widget;/);
        assert.strictEqual(record('Plain_Response').self_check, 'ok');
    });
    it('resolves a bare-identifier literal through its sibling symbol anchor', () => {
        // The literal 'Widget' must not dangle: it rides the sibling anchor's
        // module, so the emitted surface alias resolves to the real interface.
        assert.match(surface, /Lit_Sibling = .*Widget;/);
        const r = record('Lit_Sibling');
        assert.strictEqual(r.anchor_kind, 'literal');
        assert.strictEqual(r.self_check, 'ok');
        assert.strictEqual(r.source_file, '<inline>');
        // Literal anchors sit at the legacy-text tier, self-checked but counted
        // as structural_fallback so the legacy dependence stays measurable.
        assert.strictEqual(r.serialization, 'structural_fallback');
        assert.strictEqual(r.capture_failure_reason, undefined);
    });
    it('emits an inline object literal verbatim and self-checks ok', () => {
        const r = record('Lit_Object');
        assert.strictEqual(r.self_check, 'ok');
        assert.strictEqual(r.serialization, 'structural_fallback');
        assert.ok(/Lit_Object = \{\s*ok: boolean;?\s*count: number;?\s*\};/.test(surface), `surface should carry the inline object:\n${surface}`);
    });
    it('classifies a dangling bare-identifier literal as decayed, never ok', () => {
        const r = record('Lit_Dangling');
        assert.notStrictEqual(r.self_check, 'ok');
        assert.strictEqual(r.top_type_at_self_check, true);
    });
});
describe('capture v2 WP3: tsconfig-less repo fallback', () => {
    it('captures with synthesized default options when the repo has no tsconfig', () => {
        const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-wp3-notsconfig-'));
        const out = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-wp3-notsconfig-stub-'));
        try {
            fs.mkdirSync(path.join(dir, 'src'), { recursive: true });
            fs.writeFileSync(path.join(dir, 'src', 'types.ts'), 'export interface Health { status: string; }\n');
            const result = captureStub({
                repoRoot: dir,
                serviceName: 'no-tsconfig-svc',
                outDir: path.join(out, 'stub'),
                anchors: [
                    {
                        kind: 'symbol',
                        alias: 'Health_Response',
                        symbol_name: 'Health',
                        source_file: 'src/types.ts',
                        anchor_origin: 'llm-symbol',
                    },
                ],
            });
            assert.strictEqual(result.success, true, JSON.stringify(result.errors));
            const r = result.aliases.find((a) => a.alias === 'Health_Response');
            assert.ok(r);
            assert.strictEqual(r.self_check, 'ok');
        }
        finally {
            fs.rmSync(dir, { recursive: true, force: true });
            fs.rmSync(out, { recursive: true, force: true });
        }
    });
});
describe('capture v2 WP3 anchors: validator shapes', () => {
    it('accepts array_depth on symbol anchors and literal anchors', async () => {
        const { parseRequest } = await import('../src/validators.js');
        const result = parseRequest({
            action: 'capture_v2',
            request_id: 'v-1',
            repo_root: '/repo',
            service_name: 'svc',
            out_dir: '/out',
            anchors: [
                {
                    kind: 'symbol',
                    alias: 'A',
                    symbol_name: 'S',
                    source_file: 'src/a.ts',
                    anchor_origin: 'llm-symbol',
                    array_depth: 1,
                },
                {
                    kind: 'literal',
                    alias: 'B',
                    type_text: '{ id: string }',
                    anchor_origin: 'llm-symbol',
                },
            ],
        });
        assert.strictEqual(result.success, true, JSON.stringify(result));
    });
    it('rejects a literal anchor with empty type_text', async () => {
        const { parseRequest } = await import('../src/validators.js');
        const result = parseRequest({
            action: 'capture_v2',
            request_id: 'v-2',
            repo_root: '/repo',
            service_name: 'svc',
            out_dir: '/out',
            anchors: [
                { kind: 'literal', alias: 'B', type_text: '', anchor_origin: 'llm-symbol' },
            ],
        });
        assert.strictEqual(result.success, false);
    });
});
