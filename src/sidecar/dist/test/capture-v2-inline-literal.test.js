/**
 * Issue #433: producer response types declared as INLINE type literals decay
 * to `any` under bare-checkout capture.
 *
 * On a bare checkout the framework generic wrapping the literal
 * (`Response<{ ... }>`) is error-typed — its package is absent — so the
 * deterministic-infer path's checker lookup at the anchor site resolves to a
 * top type and the surface emits `export type ... = any;` (self_check
 * decayed_internal). But the literal type ARGUMENT is dependency-free source
 * syntax: only the outer generic needed the missing package.
 *
 * The fix recovers syntactically, with a purely structural trigger (no
 * framework names anywhere):
 *  - the located node's inferred type is a whole top type, AND
 *  - the governing declared annotation — found via the payload's own call
 *    (argument position or the send call's receiver chain), or via the
 *    located registration call's callback parameters — is a TypeReference
 *    whose resolution FAILED (error type) and which carries exactly one
 *    unambiguous type-literal type argument.
 * The literal node's own type (members resolve against local imports) is then
 * printed through the normal SymbolTracker-backed node-builder path, so local
 * named references ride the surface as import("...") types and the emitted
 * tree carries their declaration closure — the alias genuinely passes the
 * real self-check.
 *
 * Ambiguity refuses recovery: when more than one literal argument is in
 * scope and no payload locator disambiguates, the alias keeps decaying
 * honestly.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { captureStub } from '../src/capture/index.js';
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const BARE = path.join(__dirname, '..', '..', 'test', 'fixtures', 'capture-v2-bare');
const ROUTES = path.join(BARE, 'src', 'http', 'inline-response.ts');
const ROUTES_SOURCE = fs.readFileSync(ROUTES, 'utf8');
/** Byte span of the unique occurrence of `text` in the routes fixture. */
function spanOf(text) {
    const start = ROUTES_SOURCE.indexOf(text);
    assert.ok(start >= 0, `fixture drift: '${text}' not found`);
    assert.strictEqual(ROUTES_SOURCE.indexOf(text, start + 1), -1, `fixture drift: '${text}' is not unique`);
    return { span_start: start, span_end: start + text.length };
}
/** Byte span of the registration call starting at `head` (balanced parens). */
function registrationSpanOf(head) {
    const start = ROUTES_SOURCE.indexOf(head);
    assert.ok(start >= 0, `fixture drift: '${head}' not found`);
    const open = ROUTES_SOURCE.indexOf('(', start);
    assert.ok(open >= 0, `fixture drift: no '(' after '${head}'`);
    let depth = 0;
    for (let i = open; i < ROUTES_SOURCE.length; i++) {
        if (ROUTES_SOURCE[i] === '(')
            depth++;
        else if (ROUTES_SOURCE[i] === ')') {
            depth--;
            if (depth === 0)
                return { span_start: start, span_end: i + 1 };
        }
    }
    assert.fail(`fixture drift: unbalanced parens after '${head}'`);
}
describe('capture v2: inline literal response types under an unresolvable generic (#433)', () => {
    let result;
    let surface;
    let outRoot;
    let byAlias;
    before(() => {
        assert.ok(!fs.existsSync(path.join(BARE, 'node_modules')), 'precondition: capture-v2-bare must have no node_modules');
        outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-capture-433-'));
        result = captureStub({
            repoRoot: BARE,
            serviceName: 'inline-literal-svc',
            outDir: path.join(outRoot, 'stub'),
            anchors: [
                // Send-call shape (the express-demo evidence): the locator text is the
                // `res.json(...)` call, whole-typed `any` via the error-typed receiver.
                {
                    kind: 'infer',
                    alias: 'Endpoint_itemdetail_Response',
                    source_file: 'src/http/inline-response.ts',
                    anchor_origin: 'deterministic-infer',
                    expression_text: "res.json({ item, message: 'found' })",
                },
                // Argument-position shape: the payload identifier itself baked `any`
                // through the missing library.
                {
                    kind: 'infer',
                    alias: 'Endpoint_itemlist_Response',
                    source_file: 'src/http/inline-response.ts',
                    anchor_origin: 'deterministic-infer',
                    ...(() => {
                        const call = spanOf('res.json(items)');
                        return {
                            span_start: call.span_start + 'res.json('.length,
                            span_end: call.span_end - 1,
                        };
                    })(),
                },
                // Registration-call shape (span locator, no expression text): exactly
                // one callback parameter annotation carries an inline literal.
                {
                    kind: 'infer',
                    alias: 'Endpoint_health_Response',
                    source_file: 'src/http/inline-response.ts',
                    anchor_origin: 'deterministic-infer',
                    ...registrationSpanOf("app.get('/health'"),
                },
                // Ambiguous registration: two literal-carrying annotations, no payload
                // locator — recovery must refuse; the alias decays honestly.
                {
                    kind: 'infer',
                    alias: 'Endpoint_ambig_Response',
                    source_file: 'src/http/inline-response.ts',
                    anchor_origin: 'deterministic-infer',
                    ...registrationSpanOf("app.get('/ambig/:id'"),
                },
                // Literal referencing a third-party type: not the tractable class —
                // recovery refuses instead of baking `any` at a member.
                {
                    kind: 'infer',
                    alias: 'Endpoint_external_Response',
                    source_file: 'src/http/inline-response.ts',
                    anchor_origin: 'deterministic-infer',
                    expression_text: "res.json({ thing: makeThing('t'), ok: true })",
                },
            ],
        });
        assert.strictEqual(result.success, true, JSON.stringify(result.errors));
        surface = fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
        byAlias = new Map(result.aliases.map((a) => [a.alias, a]));
    });
    after(() => {
        if (outRoot)
            fs.rmSync(outRoot, { recursive: true, force: true });
    });
    it('recovers the send-call shape: literal with a local named type, self-check ok', () => {
        const rec = byAlias.get('Endpoint_itemdetail_Response');
        assert.strictEqual(rec.serialization, 'node_builder', rec.capture_failure_reason);
        assert.strictEqual(rec.self_check, 'ok', rec.self_check_detail);
        assert.strictEqual(rec.top_type_at_self_check, false);
        // The d.ts carries the literal's real shape: the local named type rides an
        // import("...") reference, primitives stay verbatim.
        assert.match(surface, /Endpoint_itemdetail_Response = \{\s*item: import\("[^"]*"\)\.Item;\s*message: string;\s*\}/, `surface should carry the recovered literal:\n${surface}`);
    });
    it('recovers the argument-position shape: any-typed payload identifier', () => {
        const rec = byAlias.get('Endpoint_itemlist_Response');
        assert.strictEqual(rec.serialization, 'node_builder', rec.capture_failure_reason);
        assert.strictEqual(rec.self_check, 'ok', rec.self_check_detail);
        assert.match(surface, /Endpoint_itemlist_Response = \{\s*items: import\("[^"]*"\)\.Item\[\];\s*total: number;\s*\}/, `surface should carry the recovered literal:\n${surface}`);
    });
    it('recovers the registration-call shape when exactly one literal is in scope', () => {
        const rec = byAlias.get('Endpoint_health_Response');
        assert.strictEqual(rec.serialization, 'node_builder', rec.capture_failure_reason);
        assert.strictEqual(rec.self_check, 'ok', rec.self_check_detail);
        assert.match(surface, /Endpoint_health_Response = \{\s*service: string;\s*ok: boolean;\s*\}/, `surface should carry the recovered literal:\n${surface}`);
    });
    it('refuses ambiguous recovery: two candidate literals decay honestly', () => {
        const rec = byAlias.get('Endpoint_ambig_Response');
        // The anchor located and printed — refusal happened at the recovery
        // layer, not through a locator-failure demotion.
        assert.strictEqual(rec.serialization, 'node_builder');
        assert.strictEqual(rec.capture_failure_reason, undefined);
        assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
        assert.strictEqual(rec.top_type_at_self_check, true);
        assert.match(surface, /Endpoint_ambig_Response = any;/);
    });
    it('refuses a literal referencing a third-party type: keeps decaying honestly', () => {
        const rec = byAlias.get('Endpoint_external_Response');
        assert.strictEqual(rec.serialization, 'node_builder');
        assert.strictEqual(rec.capture_failure_reason, undefined);
        assert.strictEqual(rec.self_check, 'decayed_internal', rec.self_check_detail);
        assert.strictEqual(rec.top_type_at_self_check, true);
        assert.match(surface, /Endpoint_external_Response = any;/);
    });
    it('ships the local declaration closure so the recovered aliases resolve', () => {
        assert.ok(result.emitted_files.includes('types/app/models/item.d.ts'), JSON.stringify(result.emitted_files));
        assert.strictEqual(result.fidelity.by_self_check.ok, 3);
        assert.strictEqual(result.fidelity.by_self_check.decayed_internal, 2);
    });
});
