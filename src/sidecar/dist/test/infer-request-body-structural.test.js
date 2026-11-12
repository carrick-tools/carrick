/**
 * Targeted regression for the cross-repo `POST /payments` request-body edge.
 *
 * `inferRequestBody` used to render a resolved NAMED request type with
 * `typeText`, which keeps the bare name (`CreatePayment`). The engine wrote that
 * verbatim into the cross-repo `<repo>_types.d.ts` as
 * `export type <alias> = CreatePayment;` — but those bundle files carry only
 * alias lines, no source declarations. So the name was a dangling reference →
 * resolved to `any`/`unknown` → ts_check reported `unverifiable` → `compat = None`
 * on the request edge.
 *
 * The fix expands the resolved named request type STRUCTURALLY (shared
 * `type-structural-expander.ts`, the same recovery `inferResponseBody` and
 * `inferFunctionReturn` already apply) so the bundle carries the real members
 * `{ orderId: number; amountCents: number; }`. Two shapes are covered:
 *
 *  (a) Consumer — `fetch(url, { body: JSON.stringify(payload) })` with
 *      `payload: CreatePayment`. The inferrer drills into `JSON.stringify` to
 *      the argument, whose resolved type is the named interface. No wrapper rule
 *      and no cast/binding fire, so the resolved object is expanded structurally.
 *  (b) Producer — `req.body as CreatePayment`. The cast TARGET (recovered via
 *      `extractExplicitTypeFromAncestor`) is expanded structurally; the inner
 *      `req.body` (typed `unknown`) is never emitted as the shape.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';
const FIXTURE = path.join(FIXTURES_PATH, 'src/request-body-structural.ts');
const FIXTURE_SOURCE = fs.readFileSync(FIXTURE, 'utf-8');
/** Structural form of `interface CreatePayment { orderId; amountCents }`. */
const CREATE_PAYMENT_STRUCTURAL = '{ orderId: number; amountCents: number; }';
/** Byte span of `text` in the fixture (ASCII-only file, so byte == char offsets). */
function spanOf(text) {
    const start = FIXTURE_SOURCE.indexOf(text);
    assert.ok(start >= 0, `fixture must contain: ${text}`);
    assert.strictEqual(FIXTURE_SOURCE.indexOf(text, start + 1), -1, `fixture must contain exactly one occurrence of: ${text}`);
    const line = FIXTURE_SOURCE.slice(0, start).split('\n').length;
    return { start, end: start + text.length, line };
}
describe('request_body structural expansion (cross-repo POST /payments)', () => {
    let client;
    before(async () => {
        client = new SidecarClient();
        await client.start();
        await client.send({
            action: 'init',
            request_id: 'reqbody-structural-init',
            repo_root: FIXTURES_PATH,
        });
    });
    after(async () => {
        await client.stop();
    });
    it('(a) consumer: expands `JSON.stringify(payload)` (payload: CreatePayment) to the structural shape, not the bare name', async () => {
        // Locator covers the `JSON.stringify(payload)` call. inferRequestBody drills
        // to the serialized argument `payload`, whose type is the named interface
        // `CreatePayment`. The resolved object must be expanded structurally.
        const call = spanOf('JSON.stringify(payload)');
        const response = await client.send({
            action: 'infer',
            request_id: 'reqbody-consumer-stringify',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: call.line,
                    span_start: call.start,
                    span_end: call.end,
                    infer_kind: 'request_body',
                    alias: 'ConsumerCreatePayment',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'ConsumerCreatePayment');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        // The bug: a bare `CreatePayment` (dangling in the cross-repo bundle).
        assert.notStrictEqual(inferred.type_string, 'CreatePayment', 'must not emit the bare type name — it dangles in the cross-repo bundle');
        // Must also not surface the `string` result of JSON.stringify.
        assert.notStrictEqual(inferred.type_string, 'string', 'must drill into JSON.stringify(arg), not surface its `string` result');
        // The fix: the real, self-contained member shape.
        assert.strictEqual(inferred.type_string, CREATE_PAYMENT_STRUCTURAL, `expected the structural shape, got ${inferred.type_string}`);
    });
    it('(b) producer: expands the `req.body as CreatePayment` cast target to the structural shape, not the bare name', async () => {
        // Locator covers the body expression `req.body`. inferRequestBody unwraps the
        // `as`, then recovers the cast TARGET `CreatePayment` (not the inner `unknown`)
        // and expands it structurally.
        const cast = spanOf('req.body as CreatePayment');
        const response = await client.send({
            action: 'infer',
            request_id: 'reqbody-producer-cast',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: cast.line,
                    span_start: cast.start,
                    span_end: cast.end,
                    infer_kind: 'request_body',
                    alias: 'ProducerCreatePayment',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'ProducerCreatePayment');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        // The bug: a bare `CreatePayment` (dangling), or the inner `unknown`/`any`.
        assert.notStrictEqual(inferred.type_string, 'CreatePayment', 'must not emit the bare cast-target name — it dangles in the cross-repo bundle');
        assert.ok(inferred.type_string !== 'unknown' && inferred.type_string !== 'any', `must recover the cast target, not the inner req.body type, got ${inferred.type_string}`);
        // The fix: the cast target's real member shape.
        assert.strictEqual(inferred.type_string, CREATE_PAYMENT_STRUCTURAL, `expected the structural shape, got ${inferred.type_string}`);
        assert.strictEqual(inferred.is_explicit, true, 'a recovered declared cast type must be reported explicit');
    });
});
