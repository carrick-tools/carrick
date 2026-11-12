/**
 * Targeted regression for issue #257.
 *
 * A consumer like `return res.json() as Promise<OrderView>` routes through the
 * `call_result` inference path. The inferrer used to resolve the `as
 * Promise<OrderView>` annotation to its TEXT, then string-unwrap the Promise,
 * yielding the bare type NAME `"OrderView"`. The engine wrote that verbatim
 * into the cross-repo `<repo>_types.d.ts` as `export type <alias> = OrderView;`
 * — but those bundle files carry only alias lines, no source declarations. So
 * `OrderView` was a dangling reference → resolved to `any` → ts_check reported
 * `unverifiable` → `compat = None`. The consumer's real, recoverable shape was
 * discarded.
 *
 * The fix expands the annotation's named object type STRUCTURALLY (the same
 * inliner #246 shipped for the definition-resolver, now shared via
 * `type-structural-expander.ts`), so the inferred type carries the real
 * members `{ id: string; currency: string }` and lands in the bundle as a
 * self-contained shape the type checker can actually compare.
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
describe('call_result structural expansion (#257)', () => {
    let client;
    before(async () => {
        client = new SidecarClient();
        await client.start();
        await client.send({
            action: 'init',
            request_id: 'infer257-init',
            repo_root: FIXTURES_PATH,
        });
    });
    after(async () => {
        await client.stop();
    });
    it('expands a `res.json() as Promise<T>` consumer to T\'s structural shape, not the bare name', async () => {
        // Locator covers the `res.json()` call inside
        // `return res.json() as Promise<OrderView>`. The `as Promise<OrderView>`
        // annotation must win AND be emitted structurally.
        const call = spanOf('res.json() as Promise<OrderView>');
        const response = await client.send({
            action: 'infer',
            request_id: 'infer257-call-result',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: call.line,
                    span_start: call.start,
                    span_end: call.start + 'res.json()'.length,
                    infer_kind: 'call_result',
                    alias: 'OrderViewConsumer',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'OrderViewConsumer');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        // The bug: a bare `OrderView` (dangling in the cross-repo bundle).
        assert.notStrictEqual(inferred.type_string, 'OrderView', 'must not emit the bare type name — it dangles in the cross-repo bundle');
        assert.notStrictEqual(inferred.type_string, 'Promise<OrderView>', 'Promise must be unwrapped at the type level');
        // The fix: the real, self-contained member shape.
        assert.strictEqual(inferred.type_string, '{ id: string; currency: string; }', `expected the structural shape, got ${inferred.type_string}`);
        assert.strictEqual(inferred.is_explicit, true, 'a recovered declared type must be reported explicit');
    });
});
