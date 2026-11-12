/**
 * v2 capture — all anchor forms over the stdio wire, on one bare and one
 * installed fixture (the WP1 acceptance matrix).
 *
 * Bare fixture (capture-v2-bare, no node_modules):
 *  - symbol anchors for HTTP response, HTTP request, and GraphQL op types
 *  - handler_return with the design-doc guards: the good path emits
 *    Awaited<ReturnType<...>>; overload sets and generics demote with a
 *    recorded reason (never a silently wrong .d.ts)
 *  - pub/sub payload via the symbol path through a missing external —
 *    the amendment-1 allowlist (pinned in lockfile, kept at tier)
 *  - pub/sub payload via the infer path (object-literal publish argument)
 *    printed by the SymbolTracker-backed node builder
 *  - the honest limitation: inference THROUGH the missing library bakes
 *    `any` in the emitted tree (allowlisted; probe gates are the backstop)
 *  - tsconfig-paths rewrite: `@app/*` specifiers become tree-relative
 *  - augmentation shipping: declare-global and module-augmentation files
 *    ride the tree as extra emit roots
 *  - lockfile pruning: deps never referenced by the tree are not pinned
 *
 * Installed fixture (capture-v2-installed, committed node_modules with
 * @acme/models):
 *  - self-check resolution through the repo's node_modules (symlinked into
 *    the stub for the check): externals resolve, aliases classify ok
 *  - node-builder printing of an external package type as
 *    import("@acme/models").WidgetDto
 *  - exact installed-node_modules pins for scoped packages: the fixture's
 *    package-lock.json disagrees on purpose (2.0.0) and the installed
 *    version (2.1.0) must win
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURES = path.join(__dirname, '..', '..', 'test', 'fixtures');
const BARE = path.join(FIXTURES, 'capture-v2-bare');
const INSTALLED = path.join(FIXTURES, 'capture-v2-installed');
function spanOf(file, text) {
    const source = fs.readFileSync(file, 'utf8');
    const idx = source.indexOf(text);
    assert.ok(idx >= 0, `fixture drift: '${text}' not found in ${file}`);
    return { span_start: idx, span_end: idx + text.length };
}
describe('capture_v2: all anchor forms (bare fixture)', () => {
    const client = new SidecarClient();
    let outDir;
    let response;
    let byAlias;
    before(async () => {
        assert.ok(!fs.existsSync(path.join(BARE, 'node_modules')), 'precondition: capture-v2-bare must have no node_modules');
        outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-capture-v2-forms-'));
        await client.start();
        const literal = spanOf(path.join(BARE, 'src', 'events', 'pub.ts'), "{ orderId: '1', eta: 'soon' }");
        response = (await client.send({
            request_id: 'capture-v2-forms-bare',
            action: 'capture_v2',
            repo_root: BARE,
            service_name: 'Capture Bare Svc',
            out_dir: path.join(outDir, 'stub'),
            anchors: [
                {
                    kind: 'symbol',
                    alias: 'Endpoint_order_Response',
                    symbol_name: 'OrderResponse',
                    source_file: 'src/http/routes.ts',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'symbol',
                    alias: 'Endpoint_order_Request',
                    symbol_name: 'CreateOrderRequest',
                    source_file: 'src/http/routes.ts',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'symbol',
                    alias: 'Graphql_orderquery_Result',
                    symbol_name: 'OrderQueryResult',
                    source_file: 'src/graphql/ops.ts',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'handler_return',
                    alias: 'Endpoint_getorder_Handler',
                    symbol_name: 'getOrder',
                    source_file: 'src/http/routes.ts',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'handler_return',
                    alias: 'Endpoint_overloaded_Handler',
                    symbol_name: 'overloadedHandler',
                    source_file: 'src/http/routes.ts',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'handler_return',
                    alias: 'Endpoint_generic_Handler',
                    symbol_name: 'genericHandler',
                    source_file: 'src/http/routes.ts',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'symbol',
                    alias: 'Pub_shipmentthing_Payload',
                    symbol_name: 'ShipmentThing',
                    source_file: 'src/events/pub.ts',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'symbol',
                    alias: 'Pub_derivedconfig_Payload',
                    symbol_name: 'DerivedConfigType',
                    source_file: 'src/events/pub.ts',
                    anchor_origin: 'anchor-backfill',
                },
                {
                    kind: 'infer',
                    alias: 'Pub_ordershipped_Payload',
                    source_file: 'src/events/pub.ts',
                    anchor_origin: 'deterministic-infer',
                    ...literal,
                },
                {
                    kind: 'infer',
                    alias: 'Pub_ordershipped_ByText',
                    source_file: 'src/events/pub.ts',
                    anchor_origin: 'deterministic-infer',
                    expression_text: "{ orderId: '1', eta: 'soon' }",
                },
                {
                    kind: 'infer',
                    alias: 'Pub_shipmentsync_Payload',
                    source_file: 'src/events/pub.ts',
                    anchor_origin: 'deterministic-infer',
                    expression_text: 'fetchShipment()',
                },
            ],
        }, 
        // Compiler-heavy action: CI runners need far more than the 10s default.
        120000));
        byAlias = new Map((response.result?.aliases ?? []).map((a) => [a.alias, a]));
    });
    after(async () => {
        await client.stop();
        fs.rmSync(outDir, { recursive: true, force: true });
    });
    it('succeeds and sanitizes the service name', () => {
        assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
        assert.strictEqual(response.result.package_name, '@carrick/capture-bare-svc');
        assert.strictEqual(response.result.bare_checkout, true);
    });
    it('symbol anchors (http response/request, graphql op) emit and self-check ok', () => {
        for (const alias of [
            'Endpoint_order_Response',
            'Endpoint_order_Request',
            'Graphql_orderquery_Result',
        ]) {
            const rec = byAlias.get(alias);
            assert.strictEqual(rec.serialization, 'emitted', alias);
            assert.strictEqual(rec.self_check, 'ok', `${alias}: ${rec.self_check_detail}`);
        }
    });
    it('handler_return good path emits Awaited<ReturnType<...>>', () => {
        const rec = byAlias.get('Endpoint_getorder_Handler');
        assert.strictEqual(rec.serialization, 'emitted');
        assert.strictEqual(rec.self_check, 'ok', rec.self_check_detail);
        const surface = fs.readFileSync(path.join(response.result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
        assert.match(surface, /Endpoint_getorder_Handler = Awaited<ReturnType<typeof import\(/);
    });
    it('handler_return guards demote overload sets and generics with reasons', () => {
        const overloaded = byAlias.get('Endpoint_overloaded_Handler');
        assert.strictEqual(overloaded.serialization, 'structural_fallback');
        assert.match(overloaded.capture_failure_reason ?? '', /overload set/);
        const generic = byAlias.get('Endpoint_generic_Handler');
        assert.strictEqual(generic.serialization, 'structural_fallback');
        assert.match(generic.capture_failure_reason ?? '', /generic/);
        // Demoted aliases surface as unknown, never a wrong type.
        const surface = fs.readFileSync(path.join(response.result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
        assert.match(surface, /Endpoint_overloaded_Handler = unknown/);
        assert.match(surface, /Endpoint_generic_Handler = unknown/);
    });
    it('pub/sub symbol path through a missing pinned external is allowlisted (amendment 1)', () => {
        const rec = byAlias.get('Pub_shipmentthing_Payload');
        assert.strictEqual(rec.self_check, 'allowlisted_external', rec.self_check_detail);
        assert.strictEqual(rec.serialization, 'emitted');
        assert.strictEqual(rec.top_type_at_self_check, true);
        assert.match(rec.self_check_detail ?? '', /fakelib@1\.4\.2/);
    });
    it('inference through the missing library bakes any and stays allowlisted (honest limitation)', () => {
        const rec = byAlias.get('Pub_derivedconfig_Payload');
        assert.strictEqual(rec.self_check, 'allowlisted_external', rec.self_check_detail);
        const pubDts = fs.readFileSync(path.join(response.result.stub_dir, 'types', 'events', 'pub.d.ts'), 'utf8');
        assert.match(pubDts, /DerivedConfig: any/);
        // The annotation import survives verbatim alongside.
        assert.ok(pubDts.includes('fakelib'), pubDts);
    });
    it('pub/sub infer path prints the literal payload at tier node_builder (span and text locators)', () => {
        for (const alias of ['Pub_ordershipped_Payload', 'Pub_ordershipped_ByText']) {
            const rec = byAlias.get(alias);
            assert.strictEqual(rec.serialization, 'node_builder', alias);
            assert.strictEqual(rec.self_check, 'ok', `${alias}: ${rec.self_check_detail}`);
        }
        const surface = fs.readFileSync(path.join(response.result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
        assert.match(surface, /Pub_ordershipped_Payload = \{\s*orderId: string;\s*eta: string;\s*\}/);
    });
    it('infer default unwraps Promise transport (design-doc machinery unwrap)', () => {
        const rec = byAlias.get('Pub_shipmentsync_Payload');
        assert.strictEqual(rec.serialization, 'node_builder');
        assert.strictEqual(rec.self_check, 'ok', rec.self_check_detail);
        const surface = fs.readFileSync(path.join(response.result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
        // The awaited payload, not Promise<...>.
        assert.match(surface, /Pub_shipmentsync_Payload = \{\s*shipmentId: string;\s*ok: boolean;\s*\}/);
    });
    it('rewrites tsconfig-paths specifiers to tree-relative ones', () => {
        const routesDts = fs.readFileSync(path.join(response.result.stub_dir, 'types', 'http', 'routes.d.ts'), 'utf8');
        assert.ok(!routesDts.includes('@app/'), routesDts);
        assert.ok(routesDts.includes('../app/models/item'), routesDts);
        assert.ok(response.result.specifier_rewrites >= 1);
    });
    it('ships declare-global and module-augmentation files', () => {
        const files = response.result.augmentation_files;
        assert.ok(files.includes('types/globals.d.ts'), JSON.stringify(files));
        assert.ok(files.includes('types/fakelib-augment.d.ts'), JSON.stringify(files));
        const globals = fs.readFileSync(path.join(response.result.stub_dir, 'types', 'globals.d.ts'), 'utf8');
        assert.ok(globals.includes('declare global'), globals);
    });
    it('pins referenced externals only (pruning) at exact lockfile versions', () => {
        const pins = response.result.pinned_dependencies;
        assert.strictEqual(pins['fakelib'], '1.4.2');
        assert.strictEqual(pins['leftpad-never-referenced'], undefined);
        assert.deepStrictEqual(response.result.unpinned_externals, []);
    });
    it('emits a fidelity metric that separates tiers, outcomes, and anchor origins', () => {
        const fidelity = response.result.fidelity;
        assert.strictEqual(fidelity.total_aliases, 11);
        assert.strictEqual(fidelity.by_serialization.structural_fallback, 2);
        assert.strictEqual(fidelity.by_serialization.node_builder, 3);
        assert.strictEqual(fidelity.by_serialization.emitted, 6);
        assert.strictEqual(fidelity.by_self_check.allowlisted_external, 2);
        assert.strictEqual(fidelity.by_self_check.decayed_internal, 2);
        assert.strictEqual(fidelity.by_self_check.ok, 7);
        assert.strictEqual(fidelity.by_anchor_origin['anchor-backfill'], 1);
        assert.strictEqual(fidelity.usable_rate, 0.818);
    });
});
describe('capture_v2: all anchor forms (installed fixture)', () => {
    const client = new SidecarClient();
    let outDir;
    let response;
    let byAlias;
    before(async () => {
        assert.ok(fs.existsSync(path.join(INSTALLED, 'node_modules', '@acme', 'models', 'index.d.ts')), 'precondition: capture-v2-installed must have committed node_modules');
        outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-capture-v2-forms-'));
        await client.start();
        const literal = spanOf(path.join(INSTALLED, 'src', 'service.ts'), "{ widget: w, at: 'now' }");
        response = (await client.send({
            request_id: 'capture-v2-forms-installed',
            action: 'capture_v2',
            repo_root: INSTALLED,
            service_name: 'capture-installed-svc',
            out_dir: path.join(outDir, 'stub'),
            anchors: [
                {
                    kind: 'symbol',
                    alias: 'Endpoint_widget_Response',
                    symbol_name: 'WidgetResponse',
                    source_file: 'src/service.ts',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'handler_return',
                    alias: 'Endpoint_listwidgets_Handler',
                    symbol_name: 'listWidgets',
                    source_file: 'src/service.ts',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'infer',
                    alias: 'Pub_widgetupdated_Payload',
                    source_file: 'src/service.ts',
                    anchor_origin: 'deterministic-infer',
                    ...literal,
                },
            ],
        }, 
        // Compiler-heavy action: CI runners need far more than the 10s default.
        120000));
        byAlias = new Map((response.result?.aliases ?? []).map((a) => [a.alias, a]));
    });
    after(async () => {
        await client.stop();
        fs.rmSync(outDir, { recursive: true, force: true });
    });
    it('succeeds on an installed checkout', () => {
        assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
        assert.strictEqual(response.result.bare_checkout, false);
    });
    it('self-check resolves externals through the repo node_modules: everything ok', () => {
        for (const alias of [
            'Endpoint_widget_Response',
            'Endpoint_listwidgets_Handler',
            'Pub_widgetupdated_Payload',
        ]) {
            const rec = byAlias.get(alias);
            assert.strictEqual(rec.self_check, 'ok', `${alias}: ${rec.self_check_detail}`);
            assert.strictEqual(rec.top_type_at_self_check, false, alias);
        }
        // No symlink residue in the stub package.
        assert.ok(!fs.existsSync(path.join(response.result.stub_dir, 'node_modules')));
    });
    it('node builder prints external package types as import("...") references', () => {
        const rec = byAlias.get('Pub_widgetupdated_Payload');
        assert.strictEqual(rec.serialization, 'node_builder');
        const surface = fs.readFileSync(path.join(response.result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
        assert.match(surface, /widget: import\("@acme\/models"\)\.WidgetDto/);
    });
    it('pins the scoped external at its installed version, over the disagreeing lockfile', () => {
        // node_modules/@acme/models is 2.1.0; package-lock.json says 2.0.0.
        // Installed reality wins: it is the version the repo resolves against.
        assert.strictEqual(response.result.pinned_dependencies['@acme/models'], '2.1.0');
        assert.deepStrictEqual(response.result.unpinned_externals, []);
    });
});
