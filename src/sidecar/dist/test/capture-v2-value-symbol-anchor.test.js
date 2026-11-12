/**
 * #438 part 1 (value-symbol guard) + #439 part 2 (const->type re-aim).
 *
 * An LLM symbol anchor may name a schema VALUE const
 * (`export const ZFooSchema = object({...})`) instead of its sibling
 * `export type TFoo = Infer<typeof ZFooSchema>`. Emitting the const in TYPE
 * position produces a broken surface line (a value used as a type) that
 * poisons the whole producer service. This suite pins the fix:
 *
 *  A. a value-only export with no type sibling DEMOTES (honest `unknown`, no
 *     broken surface line, reason recorded);
 *  B. a value-only export WITH a single `typeof`-derived sibling type alias
 *     RE-AIMS at that alias and captures the inferred object type;
 *  - a real type / a class (value + type meaning) still emit unchanged;
 *  - two sibling type aliases over one const are ambiguous -> demote (A).
 *
 * All fixtures are synthetic and generically named; the schema helper is a
 * local stand-in for any `object(...)` + `Infer<typeof ...>` library.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
let repoDir;
let outRoot;
const SCHEMA_TS = `
interface Schema<T> { readonly _output: T; }
declare function object<T>(shape: T): Schema<T>;
type Infer<S> = S extends Schema<infer T> ? T : never;

// Value-only const, NO sibling type alias -> part A demote.
export const ZWidgetSchema = object({ id: 0, label: '' });

// Value-only const WITH a single typeof-derived sibling -> part B re-aim.
export const ZOrderSchema = object({ orderId: '', total: 0 });
export type TOrder = Infer<typeof ZOrderSchema>;

// A real exported type -> control, must still emit unchanged.
export interface Widget { id: number; label: string; }

// A class has BOTH value and type meaning -> control, must still emit.
export class OrderModel {
  orderId = '';
  total = 0;
}

// Value-only const with TWO typeof-derived siblings -> ambiguous, demote (A).
export const ZAmbiguousSchema = object({ x: 0 });
export type TAmbiguousInput = Infer<typeof ZAmbiguousSchema>;
export type TAmbiguousOutput = Infer<typeof ZAmbiguousSchema>;

// Value-only const whose SOLE typeof-derived sibling is a WRAPPER that embeds
// the inferred payload as a member (not \`= Infer<typeof C>\`). Re-aiming would
// capture { data; meta } instead of { id; name } -> a false incompatible, so
// this must fall back to the value-only DEMOTE (abstain).
export const ZFoo = object({ id: 0, name: '' });
export type FooEnvelope = { data: Infer<typeof ZFoo>; meta: string };
`;
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
    fs.writeFileSync(path.join(repoDir, 'src', 'schema.ts'), SCHEMA_TS);
}
describe('capture v2: value-only symbol anchor guard + const->type re-aim', () => {
    let result;
    let surface;
    before(() => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-vsym-repo-'));
        outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-vsym-stub-'));
        writeRepo();
        result = captureStub({
            repoRoot: repoDir,
            serviceName: 'vsym-svc',
            outDir: path.join(outRoot, 'stub'),
            anchors: [
                {
                    kind: 'symbol',
                    alias: 'A_Widget',
                    symbol_name: 'ZWidgetSchema',
                    source_file: 'src/schema.ts',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'symbol',
                    alias: 'B_Order',
                    symbol_name: 'ZOrderSchema',
                    source_file: 'src/schema.ts',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'symbol',
                    alias: 'Ctrl_Widget',
                    symbol_name: 'Widget',
                    source_file: 'src/schema.ts',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'symbol',
                    alias: 'Ctrl_Class',
                    symbol_name: 'OrderModel',
                    source_file: 'src/schema.ts',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'symbol',
                    alias: 'Amb_Schema',
                    symbol_name: 'ZAmbiguousSchema',
                    source_file: 'src/schema.ts',
                    anchor_origin: 'llm-symbol',
                },
                {
                    kind: 'symbol',
                    alias: 'Wrapper_Foo',
                    symbol_name: 'ZFoo',
                    source_file: 'src/schema.ts',
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
    it('A: a value-only const with no type sibling is demoted, not emitted broken', () => {
        const r = record('A_Widget');
        assert.strictEqual(r.serialization, 'structural_fallback');
        assert.strictEqual(r.self_check, 'decayed_internal');
        assert.ok(r.capture_failure_reason, 'demotion reason must be recorded');
        assert.match(r.capture_failure_reason, /value export with no type-space meaning/);
        assert.strictEqual(r.top_type_at_self_check, true);
        // The surface line is honest `unknown` — never a value in type position.
        assert.match(surface, /A_Widget = unknown;/);
        assert.ok(!/A_Widget = .*ZWidgetSchema/.test(surface), `A_Widget must not reference the value const:\n${surface}`);
    });
    it('B: a value-only const re-aims at its typeof sibling and captures the payload', () => {
        const r = record('B_Order');
        assert.strictEqual(r.serialization, 'emitted', r.self_check_detail);
        assert.strictEqual(r.self_check, 'ok', r.self_check_detail);
        // Concrete resolution (the inferred object, not a decayed top type).
        assert.strictEqual(r.top_type_at_self_check, false);
        // The surface redirects from the const to the sibling type alias.
        assert.match(surface, /B_Order = .*TOrder/, `B_Order must reference the sibling type alias:\n${surface}`);
        assert.ok(!/B_Order = .*ZOrderSchema/.test(surface), `B_Order must not reference the value const:\n${surface}`);
        assert.match(r.self_check_detail ?? '', /re-aimed at sibling type alias 'TOrder'/);
    });
    it('B captures the inferred object shape (orderId/total) in the tree', () => {
        // The sibling alias resolves to the object the schema describes; the
        // emitted tree carries that inferred shape (not the schema wrapper).
        const treeText = result.emitted_files
            .map((rel) => fs.readFileSync(path.join(result.stub_dir, rel), 'utf8'))
            .join('\n');
        assert.match(treeText, /orderId/);
        assert.match(treeText, /total/);
    });
    it('control: a real exported type still emits unchanged', () => {
        const r = record('Ctrl_Widget');
        assert.strictEqual(r.serialization, 'emitted');
        assert.strictEqual(r.self_check, 'ok', r.self_check_detail);
        assert.match(surface, /Ctrl_Widget = .*Widget;/);
    });
    it('control: a class (value + type meaning) still emits unchanged', () => {
        const r = record('Ctrl_Class');
        assert.strictEqual(r.serialization, 'emitted', r.self_check_detail);
        assert.strictEqual(r.self_check, 'ok', r.self_check_detail);
        assert.ok(!/Ctrl_Class = unknown;/.test(surface), `a class anchor must not demote:\n${surface}`);
    });
    it('B ambiguity: two typeof siblings over one const refuse re-aim and demote', () => {
        const r = record('Amb_Schema');
        assert.strictEqual(r.serialization, 'structural_fallback');
        assert.strictEqual(r.self_check, 'decayed_internal');
        assert.ok(r.capture_failure_reason, 'ambiguous re-aim must demote with a reason');
        assert.match(surface, /Amb_Schema = unknown;/);
    });
    it('B wrapper: a sibling that EMBEDS the inferred type (not `= Infer<...>`) demotes', () => {
        // FooEnvelope = { data: Infer<typeof ZFoo>; meta: string } is a wrapper:
        // re-aiming would capture the envelope shape and self-check concretely,
        // manufacturing a false incompatible. The guard requires the RHS to BE the
        // inferred type, so this abstains (demotes) instead.
        const r = record('Wrapper_Foo');
        assert.strictEqual(r.serialization, 'structural_fallback');
        assert.strictEqual(r.self_check, 'decayed_internal');
        assert.ok(r.capture_failure_reason, 'wrapper sibling must demote with a reason');
        assert.match(surface, /Wrapper_Foo = unknown;/);
        // Never captures the wrapper's members.
        assert.ok(!/Wrapper_Foo = [\s\S]*FooEnvelope/.test(surface), surface);
        assert.ok(!/Wrapper_Foo = [\s\S]*meta/.test(surface), surface);
    });
});
