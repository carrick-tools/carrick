/**
 * Targeted regression for issues #257 (producer expansion) and #240
 * (deterministic anchor), the PRODUCER-side analogues of #259's consumer fix.
 *
 * A producer `res.json(payment)` where `payment: Payment` routes through the
 * `response_body` inference path. The inferrer used to render the resolved
 * payload type with `typeText`, which keeps the bare NAME `Payment`. The engine
 * wrote that verbatim into the cross-repo `<repo>_types.d.ts` as
 * `export type <alias> = Payment;` — but those bundle files carry only alias
 * lines, no source declarations. So `Payment` was a dangling reference →
 * resolved to `any` → ts_check reported `unverifiable` → `compat = None`,
 * masking the producer's real, recoverable shape (deterministically explaining
 * `payments-svc|POST|/payments -> web-frontend` reading `None`).
 *
 * The fix:
 *  (a) expands the resolved named object STRUCTURALLY (shared
 *      `type-structural-expander.ts`), so the bundle carries the real members
 *      `{ id: string; amountCents: number; currency: string }`; and
 *  (b) carries the deterministic source symbol on `primary_type_symbol`
 *      (`Payment`), derived from the ts-morph `Type` symbol with TS/lib globals
 *      filtered out, so the manifest anchor no longer depends on the LLM.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';
const FIXTURE = path.join(FIXTURES_PATH, 'src/fetch-json.ts');
const FIXTURE_SOURCE = fs.readFileSync(FIXTURE, 'utf-8');
/** Byte span of `text` in the fixture (ASCII-only file, so byte == char offsets). */
function spanOf(text) {
    const start = FIXTURE_SOURCE.indexOf(text);
    assert.ok(start >= 0, `fixture must contain: ${text}`);
    assert.strictEqual(FIXTURE_SOURCE.indexOf(text, start + 1), -1, `fixture must contain exactly one occurrence of: ${text}`);
    const line = FIXTURE_SOURCE.slice(0, start).split('\n').length;
    return { start, end: start + text.length, line };
}
describe('response_body producer structural expansion + anchor (#257 #240)', () => {
    let client;
    before(async () => {
        client = new SidecarClient();
        await client.start();
        await client.send({
            action: 'init',
            request_id: 'producer-init',
            repo_root: FIXTURES_PATH,
        });
    });
    after(async () => {
        await client.stop();
    });
    it('expands `res.json(payment)` (payment: Payment) to the structural shape, not the bare name, and carries the anchor symbol', async () => {
        // Locator covers the `res.json(payment)` call. inferResponseBody drills to
        // the first argument `payment`, whose type is the named interface `Payment`.
        const call = spanOf('res.json(payment)');
        const response = await client.send({
            action: 'infer',
            request_id: 'producer-response-body',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: call.line,
                    span_start: call.start,
                    span_end: call.end,
                    infer_kind: 'response_body',
                    alias: 'PaymentProducer',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'PaymentProducer');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        // (a) The bug: a bare `Payment` (dangling in the cross-repo bundle).
        assert.notStrictEqual(inferred.type_string, 'Payment', 'must not emit the bare type name — it dangles in the cross-repo bundle');
        // (a) The fix: the real, self-contained member shape.
        assert.strictEqual(inferred.type_string, '{ id: string; amountCents: number; currency: string; }', `expected the structural shape, got ${inferred.type_string}`);
        // (b) The deterministic anchor: the real source symbol, not a hash.
        assert.strictEqual(inferred.primary_type_symbol, 'Payment', `expected primary_type_symbol "Payment", got ${inferred.primary_type_symbol}`);
    });
});
