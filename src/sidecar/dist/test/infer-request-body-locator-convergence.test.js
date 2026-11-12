/**
 * Regression for `inferRequestBody` TEXT-LOCATOR convergence.
 *
 * The scanner sends the LLM's `expression_text` + `expression_line` locator
 * (not a byte span) for cross-repo consumer request bodies. Two locator
 * shapes used to resolve the WRONG node and surface the useless `string`
 * result of `JSON.stringify` instead of the payload:
 *
 *  1. A bare property-name locator (`text="body"`) on a `{ body:
 *     JSON.stringify(body) }` property line exact-matches the property NAME
 *     identifier — which types as the assigned value (`string`), not the
 *     payload.
 *  2. A whole-property locator (`text="body: JSON.stringify(body)"`)
 *     resolves the PropertyAssignment node itself, which also types as its
 *     value.
 *  3. A locator on a serialized IDENTIFIER one value-hop from the payload
 *     (`const body = JSON.stringify(payload); sendBeacon(url, body)`)
 *     resolves the argument identifier, whose own declared type is `string`.
 *
 * The fix adds two structural redirects in `inferRequestBody`: (1) a
 * PropertyAssignment / property-name-identifier locator redirects to the
 * property's value; (2) a serialized identifier follows ONE hop to its
 * declaration and, only when that declaration's initializer is itself a
 * `JSON.stringify(...)` call, resolves through to the serialized argument.
 * Cases 4 and 5 are controls: case 4 proves the pre-existing direct-call path
 * still works; case 5 proves the one-hop follow does not fire on a
 * non-stringify initializer.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';
const FIXTURE = path.join(FIXTURES_PATH, 'src/request-body-locator-convergence.ts');
const FIXTURE_SOURCE = fs.readFileSync(FIXTURE, 'utf-8');
/** Structural form of `interface CreatePaymentRequest { orderId; amountCents }`. */
const CREATE_PAYMENT_STRUCTURAL = '{ orderId: number; amountCents: number; }';
/** Structural form of `interface MetricPayload { event; paymentId; durationMs }`. */
const METRIC_PAYLOAD_STRUCTURAL = '{ event: string; paymentId: string; durationMs: number; }';
/**
 * 1-based line number of the (unique) line containing `anchorText`. Used to
 * build `expression_line` the same way the scanner's LLM locator does — a
 * line number alongside `expression_text`, not a byte span.
 */
function lineOf(anchorText) {
    const idx = FIXTURE_SOURCE.indexOf(anchorText);
    assert.ok(idx >= 0, `fixture must contain: ${anchorText}`);
    assert.strictEqual(FIXTURE_SOURCE.indexOf(anchorText, idx + 1), -1, `fixture must contain exactly one occurrence of: ${anchorText}`);
    return FIXTURE_SOURCE.slice(0, idx).split('\n').length;
}
describe('request_body TEXT-LOCATOR convergence (property-name / serialized-identifier)', () => {
    let client;
    before(async () => {
        client = new SidecarClient();
        await client.start();
        await client.send({
            action: 'init',
            request_id: 'reqbody-locator-init',
            repo_root: FIXTURES_PATH,
        });
    });
    after(async () => {
        await client.stop();
    });
    async function inferOne(alias, expressionText, expressionLine) {
        const response = await client.send({
            action: 'infer',
            request_id: `reqbody-locator-${alias}`,
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: expressionLine,
                    expression_text: expressionText,
                    expression_line: expressionLine,
                    infer_kind: 'request_body',
                    alias,
                },
            ],
        });
        assert.ok(response.inferred_types?.length || response.errors?.length, `expected either an inferred type or an error, got neither: ${JSON.stringify(response)}`);
        return response.inferred_types?.find((t) => t.alias === alias);
    }
    it('1. bare property-name locator ("body" on the property line) resolves the payload shape, not `string`', async () => {
        const line = lineOf('body: JSON.stringify(body)');
        const inferred = await inferOne('PropertyNameLocator', 'body', line);
        assert.ok(inferred, 'expected an inferred type');
        assert.notStrictEqual(inferred.type_string, 'string', 'must not surface the property NAME identifier\'s type (JSON.stringify\'s `string` result)');
        assert.strictEqual(inferred.type_string, CREATE_PAYMENT_STRUCTURAL);
    });
    it('2. whole-property locator ("body: JSON.stringify(body)") resolves the payload shape, not `string`', async () => {
        const line = lineOf('body: JSON.stringify(body)');
        const inferred = await inferOne('PropertyAssignmentLocator', 'body: JSON.stringify(body)', line);
        assert.ok(inferred, 'expected an inferred type');
        assert.notStrictEqual(inferred.type_string, 'string', 'must not surface the PropertyAssignment node\'s own type (its value\'s `string`)');
        assert.strictEqual(inferred.type_string, CREATE_PAYMENT_STRUCTURAL);
    });
    it('3. serialized-identifier locator ("body" at the sendBeacon call line) resolves the payload shape, not `string`', async () => {
        const line = lineOf("navigator.sendBeacon('/metrics/ingest', body)");
        const inferred = await inferOne('SerializedIdentifierLocator', 'body', line);
        assert.ok(inferred, 'expected an inferred type');
        assert.notStrictEqual(inferred.type_string, 'string', 'must not surface the argument identifier\'s own declared type (`string`) — follow one hop to the JSON.stringify argument');
        assert.strictEqual(inferred.type_string, METRIC_PAYLOAD_STRUCTURAL);
    });
    it('4. control: direct `JSON.stringify(body)` call locator keeps resolving the payload shape', async () => {
        const line = lineOf('body: JSON.stringify(body)');
        const inferred = await inferOne('DirectStringifyControl', 'JSON.stringify(body)', line);
        assert.ok(inferred, 'expected an inferred type');
        assert.strictEqual(inferred.type_string, CREATE_PAYMENT_STRUCTURAL);
    });
    it('5. control: an identifier declared from a NON-stringify initializer is not rewritten by the one-hop follow', async () => {
        const line = lineOf("navigator.sendBeacon('/metrics/raw', raw)");
        const inferred = await inferOne('NonStringifyIdentifierControl', 'raw', line);
        assert.ok(inferred, 'expected an inferred type');
        // `raw` is declared `const raw = getRawPayload()` — a non-stringify call.
        // The one-hop follow must not fire; the identifier's own type (`string`)
        // is correct here and must be preserved faithfully.
        assert.strictEqual(inferred.type_string, 'string');
    });
});
