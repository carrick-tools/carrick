/**
 * #306: an inference that resolves to an array type must anchor on the ELEMENT
 * symbol and report the peeled depth. An array type's own symbol is the builtin
 * `Array` (filtered), so before this the anchor was silently dropped — and,
 * worse, an explicit anchor bundled for the same alias erased the use-site's
 * array-ness, scoring an array-vs-scalar mismatch as compatible.
 *
 * `array_depth` is what `resolve_all_types` (Rust) copies onto the matching
 * explicit `SymbolRequest`, so the bundler's existing array wrap (#248)
 * restores the `[]` levels on the producer comparand.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';
const HANDLERS_FIXTURE = path.join(FIXTURES_PATH, 'src/framework-handlers.ts');
describe('array-typed inference anchors on the element symbol with array_depth (#306)', () => {
    let client;
    before(async () => {
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'arr-anchor-init', repo_root: FIXTURES_PATH });
    });
    after(async () => {
        await client.stop();
    });
    it('function_return of `User[]` → element anchor `User`, array_depth 1', async () => {
        const response = await client.send({
            action: 'infer',
            request_id: 'arr-anchor-fn-return',
            requests: [
                {
                    file_path: HANDLERS_FIXTURE,
                    line_number: 47,
                    expression_text: 'latest',
                    expression_line: 48,
                    infer_kind: 'function_return',
                },
            ],
        });
        assert.ok(response.inferred_types && response.inferred_types.length > 0, `expected inferred_types, got ${JSON.stringify(response)}`);
        const inferred = response.inferred_types[0];
        assert.strictEqual(inferred.type_string, '{ id: number; name: string; }[]', `expected structural User[], got ${inferred.type_string}`);
        assert.strictEqual(inferred.primary_type_symbol, 'User', `anchor must be the element symbol, got ${JSON.stringify(inferred.primary_type_symbol)}`);
        assert.strictEqual(inferred.array_depth, 1);
    });
    it('response_body payload `users: User[]` → element anchor `User`, array_depth 1', async () => {
        const response = await client.send({
            action: 'infer',
            request_id: 'arr-anchor-response-body',
            requests: [
                {
                    file_path: HANDLERS_FIXTURE,
                    line_number: 23,
                    expression_text: 'users',
                    expression_line: 23,
                    infer_kind: 'response_body',
                },
            ],
        });
        assert.ok(response.inferred_types && response.inferred_types.length > 0, `expected inferred_types, got ${JSON.stringify(response)}`);
        const inferred = response.inferred_types[0];
        assert.strictEqual(inferred.primary_type_symbol, 'User', `anchor must be the element symbol, got ${JSON.stringify(inferred.primary_type_symbol)}`);
        assert.strictEqual(inferred.array_depth, 1);
    });
    it('scalar return keeps its anchor and reports NO array_depth', async () => {
        const response = await client.send({
            action: 'infer',
            request_id: 'arr-anchor-scalar',
            requests: [
                {
                    file_path: HANDLERS_FIXTURE,
                    line_number: 39,
                    expression_text: 'user',
                    expression_line: 40,
                    infer_kind: 'function_return',
                },
            ],
        });
        assert.ok(response.inferred_types && response.inferred_types.length > 0, `expected inferred_types, got ${JSON.stringify(response)}`);
        const inferred = response.inferred_types[0];
        assert.strictEqual(inferred.primary_type_symbol, 'User');
        assert.strictEqual(inferred.array_depth, undefined, `a scalar must not report an array_depth, got ${inferred.array_depth}`);
    });
});
