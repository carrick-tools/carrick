/**
 * v2 capture ("tsc as the serializer") — bare-checkout stdio test.
 *
 * Drives the sidecar over the same JSON-over-stdio wire the Rust client uses
 * (the integration point for src/services/type_sidecar.rs), against the
 * xrepo-corpus-3 inventory-svc fixture, which has NO node_modules — the bare
 * checkout case from the v2 design study.
 *
 * What this pins:
 *  1. `tsc --noCheck --declaration --emitDeclarationOnly` capture succeeds on
 *     a bare checkout (exit clean, tree emitted).
 *  2. Third-party references survive as syntactic import references
 *     (`import { z } from "zod"` + `z.infer<typeof StockAdjustSchema>`
 *     verbatim in the emitted tree) where expandTypeStructural bakes `any`.
 *  3. In-repo ambient declaration sources (the fixture's stubs.d.ts with
 *     `declare module "zod"` — the corpus determinism recipe) ship verbatim:
 *     tsc never re-emits input .d.ts, so without the copy the tree's
 *     references dangle. With them shipped the stub is self-contained and
 *     the zod-derived alias self-checks ok even on a bare checkout.
 *  4. External deps referenced by the tree pin to exact lockfile versions,
 *     pruned to what the tree actually references.
 *  5. anchor_origin (amendment 2) rides every alias record, and the
 *     per-capture fidelity metric is emitted.
 *
 * The amendment-1 allowlist path (pinned external, unresolved on bare, NO
 * ambient stub) is pinned by capture-v2-forms.test.ts on a purpose-built
 * fixture; this corpus fixture stubs its externals so it cannot exercise it.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';
const __dirname = path.dirname(fileURLToPath(import.meta.url));
// dist/test -> repo root is four levels up (dist/test -> dist -> sidecar -> src -> root)
const FIXTURE = path.join(__dirname, '..', '..', '..', '..', 'tests', 'fixtures', 'xrepo-corpus-3', 'inventory-svc');
describe('capture_v2: bare-checkout tsc-as-serializer (corpus-3 fixture)', () => {
    const client = new SidecarClient();
    let outDir;
    let response;
    before(async () => {
        assert.ok(fs.existsSync(FIXTURE), `fixture missing: ${FIXTURE}`);
        assert.ok(!fs.existsSync(path.join(FIXTURE, 'node_modules')), 'precondition: fixture must be a bare checkout (no node_modules)');
        outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-capture-v2-test-'));
        await client.start();
        response = (await client.send({
            request_id: 'capture-v2-corpus3-1',
            action: 'capture_v2',
            repo_root: FIXTURE,
            service_name: 'inventory-svc',
            out_dir: path.join(outDir, 'stub'),
            anchors: [
                {
                    kind: 'symbol',
                    alias: 'Endpoint_stock_Response',
                    symbol_name: 'StockLevel',
                    source_file: 'src/types/stock.ts',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'symbol',
                    alias: 'Sub_stockadjust_Payload',
                    symbol_name: 'StockAdjustCommand',
                    source_file: 'src/types/stock.ts',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'symbol',
                    alias: 'Sub_priceupdated_Payload',
                    symbol_name: 'PriceUpdatedEvent',
                    source_file: 'src/types/stock.ts',
                    anchor_origin: 'deterministic-infer',
                },
            ],
        }, 
        // Compiler-heavy action: CI runners need far more than the 10s default.
        120000));
    });
    after(async () => {
        await client.stop();
        fs.rmSync(outDir, { recursive: true, force: true });
    });
    it('succeeds on a bare checkout with clean emit', () => {
        assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
        assert.ok(response.result);
        assert.strictEqual(response.result.bare_checkout, true);
        assert.deepStrictEqual(response.result.errors, []);
    });
    it('emits the v2 stub package shape', () => {
        const result = response.result;
        assert.strictEqual(result.package_name, '@carrick/inventory-svc');
        assert.ok(result.emitted_files.includes('types/surface.d.ts'));
        assert.ok(result.emitted_files.includes('types/src/types/stock.d.ts'));
        assert.ok(fs.existsSync(path.join(result.stub_dir, 'package.json')));
        assert.ok(fs.existsSync(path.join(result.stub_dir, 'tsconfig.snapshot.json')));
        assert.ok(fs.existsSync(path.join(result.stub_dir, 'carrick-manifest.json')));
        const pkg = JSON.parse(fs.readFileSync(path.join(result.stub_dir, 'package.json'), 'utf8'));
        assert.strictEqual(pkg.types, './types/surface.d.ts');
    });
    it('preserves third-party references as import references (not any)', () => {
        const result = response.result;
        const stockDts = fs.readFileSync(path.join(result.stub_dir, 'types', 'src', 'types', 'stock.d.ts'), 'utf8');
        // The exact loss expandTypeStructural forces today: these survive verbatim.
        assert.ok(stockDts.includes('import { z } from "zod"'), stockDts);
        assert.ok(stockDts.includes('z.infer<typeof StockAdjustSchema>'), stockDts);
        // The schema const keeps a real zod type reference instead of baking any:
        // the fixture's ambient stubs are part of the repo's own compilation, so
        // the capture program sees them exactly like the repo's tsc does.
        assert.match(stockDts, /StockAdjustSchema: import\("zod"\)\./, stockDts);
    });
    it('ships in-repo ambient declaration sources verbatim', () => {
        const result = response.result;
        assert.ok(result.emitted_files.includes('types/src/types/stubs.d.ts'), JSON.stringify(result.emitted_files));
        const stubs = fs.readFileSync(path.join(result.stub_dir, 'types', 'src', 'types', 'stubs.d.ts'), 'utf8');
        assert.ok(stubs.includes('declare module "zod"'), 'ambient module text must survive');
    });
    it('pins referenced externals to exact lockfile versions', () => {
        const result = response.result;
        assert.strictEqual(result.pinned_dependencies['zod'], '3.23.0');
        assert.deepStrictEqual(result.unpinned_externals, []);
    });
    it('self-check: every alias resolves in the self-contained tree', () => {
        const byAlias = new Map(response.result.aliases.map((a) => [a.alias, a]));
        for (const alias of [
            'Endpoint_stock_Response',
            'Sub_stockadjust_Payload',
            'Sub_priceupdated_Payload',
        ]) {
            const rec = byAlias.get(alias);
            assert.strictEqual(rec.self_check, 'ok', `${alias}: ${rec.self_check_detail}`);
            assert.strictEqual(rec.top_type_at_self_check, false, alias);
            assert.strictEqual(rec.serialization, 'emitted');
        }
    });
    it('records anchor provenance separately from the serialization tier (amendment 2)', () => {
        const byAlias = new Map(response.result.aliases.map((a) => [a.alias, a]));
        assert.strictEqual(byAlias.get('Endpoint_stock_Response').anchor_origin, 'llm-symbol');
        assert.strictEqual(byAlias.get('Sub_priceupdated_Payload').anchor_origin, 'deterministic-infer');
    });
    it('emits the per-capture fidelity metric', () => {
        const fidelity = response.result.fidelity;
        assert.strictEqual(fidelity.total_aliases, 3);
        assert.strictEqual(fidelity.by_serialization.emitted, 3);
        assert.strictEqual(fidelity.by_self_check.ok, 3);
        assert.strictEqual(fidelity.by_anchor_origin['llm-symbol'], 2);
        assert.strictEqual(fidelity.by_anchor_origin['deterministic-infer'], 1);
        assert.strictEqual(fidelity.usable_rate, 1);
    });
});
