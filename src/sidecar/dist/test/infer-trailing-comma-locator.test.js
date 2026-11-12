/**
 * Regression for #335: a trailing comma defeats matchByText and the producer
 * response type is captured as the string literal "active".
 *
 * The source `res.json({...})` payload is a multi-line object literal with a
 * trailing comma (`userCount: userCount,`). The LLM's `response_expression_text`
 * is the same expression printed single-line WITHOUT the trailing comma.
 * `normalizeWhitespace` only collapsed whitespace, so:
 *
 *  1. Exact match failed on the comma, for the object literal and for every
 *     enclosing node (`res.json(...)`, the statement, ...).
 *  2. Forward containment (node text includes target) failed the same way.
 *  3. The reverse-substring branch (target includes node text, min length 8)
 *     matched many tiny sub-expressions: `"active"` (8 chars), `userCount`,
 *     `status: "active"`, `new Date().toISOString()`, ...
 *  4. `pickBestMatch` prefers the smallest node, so `"active"` won,
 *     deterministically, and every consumer of the endpoint got a permanent
 *     CAUTION contract risk (`producer "active" vs consumer ...`).
 *
 * The fix strips trailing commas before `}` `)` `]` in the normalization step
 * (applied symmetrically to node text and target, so the exact match succeeds)
 * and requires a reverse-substring match to cover at least half of the target,
 * so a 100+ char locator can never bind to an 8-char fragment.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';
const FIXTURE = path.join(FIXTURES_PATH, 'src/trailing-comma-locator.ts');
const FIXTURE_SOURCE = fs.readFileSync(FIXTURE, 'utf-8');
/**
 * The locator exactly as the LLM reports it: the multi-line source expression
 * printed on a single line, without the trailing comma before `}`.
 */
const LLM_LOCATOR_TEXT = 'res.json({ service: "notification-service", status: "active", timestamp: new Date().toISOString(), userCount: userCount })';
/** 1-based line number of the (unique) line containing `anchorText`. */
function lineOf(anchorText) {
    const idx = FIXTURE_SOURCE.indexOf(anchorText);
    assert.ok(idx >= 0, `fixture must contain: ${anchorText}`);
    assert.strictEqual(FIXTURE_SOURCE.indexOf(anchorText, idx + 1), -1, `fixture must contain exactly one occurrence of: ${anchorText}`);
    return FIXTURE_SOURCE.slice(0, idx).split('\n').length;
}
describe('response_body text locator vs trailing comma (#335)', () => {
    let client;
    before(async () => {
        client = new SidecarClient();
        await client.start();
        await client.send({
            action: 'init',
            request_id: 'trailing-comma-init',
            repo_root: FIXTURES_PATH,
        });
    });
    after(async () => {
        await client.stop();
    });
    it('resolves the object literal payload, not the embedded "active" literal', async () => {
        const line = lineOf('res.json({');
        const response = await client.send({
            action: 'infer',
            request_id: 'trailing-comma-producer',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: line,
                    expression_text: LLM_LOCATOR_TEXT,
                    expression_line: line,
                    infer_kind: 'response_body',
                    alias: 'StatusProducer',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'StatusProducer');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        // The bug: the smallest-node fallback bound the locator to `"active"`.
        assert.notStrictEqual(inferred.type_string, '"active"', 'locator must not bind to the embedded string literal "active"');
        // The fix: the payload resolves to the full object shape.
        assert.strictEqual(inferred.type_string, '{ service: string; status: string; timestamp: string; userCount: number; }', `expected the payload object shape, got ${inferred.type_string}`);
    });
});
