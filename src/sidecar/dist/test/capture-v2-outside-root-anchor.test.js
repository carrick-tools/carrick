/**
 * An anchor whose file lies outside the service root arrives as an ABSOLUTE
 * path (the scanner leaves a path outside the capture root as it is). The
 * capture joined every `source_file` onto the root with `path.join`, which
 * nests `/abs` under the root, so the file was "not in program" and the alias
 * demoted. A source file resolves with `path.resolve`: an absolute path stays
 * absolute and a relative one is unchanged.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
function write(root, rel, text) {
    const file = path.join(root, rel);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, text);
}
function surfaceLine(surface, alias) {
    const start = surface.indexOf(`export type ${alias} =`);
    assert.ok(start >= 0, `no surface line for ${alias}:\n${surface}`);
    const next = surface.indexOf('\nexport type ', start + 1);
    return surface.slice(start, next < 0 ? undefined : next).replace(/\s+/g, ' ').trim();
}
const DTO = 'export interface OrderDto { id: string; legacyCode: string }\nexport const ORDER_STATUS = { open: 1 } as const;\n';
/** `services/orders-api` imports `packages/shop-contracts` by a relative path
 * or through a workspace link made here. */
function capture(scratch, name, link, missing = false) {
    const workspace = path.join(scratch, name);
    const repoRoot = path.join(workspace, 'services', 'orders-api');
    const sibling = path.join(workspace, 'packages', 'shop-contracts', 'src', 'index.ts');
    write(workspace, 'packages/shop-contracts/src/index.ts', DTO);
    write(workspace, 'packages/shop-contracts/package.json', JSON.stringify({
        name: '@shop/contracts', version: '1.0.0', types: './src/index.ts',
    }));
    write(repoRoot, 'tsconfig.json', JSON.stringify({
        compilerOptions: {
            target: 'ES2022', module: 'ESNext', moduleResolution: 'Bundler',
            strict: true, skipLibCheck: true, rootDir: 'src',
        },
        include: ['src'],
    }));
    write(repoRoot, 'package.json', JSON.stringify({ name: 'orders-api', version: '0.0.0' }));
    const specifier = link ? '@shop/contracts' : '../../../packages/shop-contracts/src/index';
    if (link) {
        fs.mkdirSync(path.join(repoRoot, 'node_modules', '@shop'), { recursive: true });
        fs.symlinkSync(path.join(workspace, 'packages', 'shop-contracts'), path.join(repoRoot, 'node_modules', '@shop', 'contracts'));
    }
    write(repoRoot, 'src/orders.ts', [
        `import type { OrderDto } from '${specifier}';`,
        'export function load(): OrderDto { return { id: "1", legacyCode: "x" }; }',
        'export interface OrderRow { id: string }',
        '',
    ].join('\n'));
    const absolute = missing ? path.join(workspace, 'packages', 'gone', 'index.ts') : sibling;
    const anchors = [
        { kind: 'symbol', alias: 'Abs_Symbol', symbol_name: 'OrderDto', source_file: absolute, anchor_origin: 'llm-symbol' },
        {
            kind: 'literal', alias: 'Abs_Literal', type_text: '{ dto: OrderDto; s: typeof ORDER_STATUS }',
            source_file: absolute, anchor_origin: 'deterministic-infer',
        },
        {
            kind: 'literal', alias: 'Rel_Literal', type_text: '{ dto: OrderDto; s: typeof ORDER_STATUS }',
            source_file: path.relative(repoRoot, absolute), anchor_origin: 'deterministic-infer',
        },
        {
            kind: 'infer', alias: 'Abs_Infer', source_file: absolute, line_number: 1,
            anchor_origin: 'deterministic-infer',
        },
        { kind: 'symbol', alias: 'Rel_Symbol', symbol_name: 'OrderRow', source_file: 'src/orders.ts', anchor_origin: 'llm-symbol' },
    ];
    const result = captureStub({
        repoRoot, serviceName: name, outDir: path.join(scratch, `${name}-stub`), anchors,
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    return {
        result,
        records: new Map(result.aliases.map((r) => [r.alias, r])),
        surface: fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8'),
        sibling,
    };
}
describe('an absolute source_file outside the service root resolves as itself', () => {
    let scratch;
    const runs = new Map();
    before(() => {
        scratch = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-outside-anchor-')));
        runs.set('relative', capture(scratch, 'rel-ws', false));
        runs.set('linked', capture(scratch, 'link-ws', true));
        runs.set('missing', capture(scratch, 'gone-ws', false, true));
    });
    after(() => fs.rmSync(scratch, { recursive: true, force: true }));
    for (const variant of ['relative', 'linked']) {
        describe(`${variant} import`, () => {
            it('a symbol anchor self-checks ok and imports from __outside__', () => {
                const run = runs.get(variant);
                const record = run.records.get('Abs_Symbol');
                assert.strictEqual(record.capture_failure_reason, undefined, record.capture_failure_reason);
                assert.strictEqual(record.self_check, 'ok', record.self_check_detail);
                assert.match(surfaceLine(run.surface, 'Abs_Symbol'), /import\(["']\.\/__outside__\//);
            });
            it('a literal anchor with the absolute file reads like the same file given relative', () => {
                const run = runs.get(variant);
                const abs = run.records.get('Abs_Literal');
                const rel = run.records.get('Rel_Literal');
                assert.strictEqual(abs.capture_failure_reason, rel.capture_failure_reason);
                assert.strictEqual(abs.self_check, rel.self_check);
                assert.strictEqual(surfaceLine(run.surface, 'Abs_Literal').replace('Abs_Literal', 'X'), surfaceLine(run.surface, 'Rel_Literal').replace('Rel_Literal', 'X'));
            });
            it('an infer anchor at a line of the sibling file locates', () => {
                const run = runs.get(variant);
                const record = run.records.get('Abs_Infer');
                assert.doesNotMatch(record.capture_failure_reason ?? '', /not in program/);
            });
        });
    }
    it('an absolute file that is not in the program still demotes with the same reason', () => {
        const run = runs.get('missing');
        const record = run.records.get('Abs_Symbol');
        assert.match(record.capture_failure_reason ?? '', /^source file not in program: \//);
    });
    it('an in-root relative anchor is unchanged', () => {
        for (const variant of ['relative', 'linked']) {
            const run = runs.get(variant);
            const record = run.records.get('Rel_Symbol');
            assert.strictEqual(record.capture_failure_reason, undefined);
            assert.match(surfaceLine(run.surface, 'Rel_Symbol'), /import\(["']\.\/orders["']\)\.OrderRow/);
        }
    });
});
