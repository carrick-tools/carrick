/**
 * Targeted regression for issue #133 root cause A1.
 *
 * `inferRequestBody` used to read the raw type of the located node
 * (`request.json()` / `await request.json()`), surfacing `Promise<any>` /
 * `any` even when the caller declared the body type via an `as T` cast or a
 * typed variable binding. The fix mirrors `inferCallResult`: unwrap the
 * `await`/`as`/paren/`!` wrappers and let an explicit `as T` cast or typed
 * binding/annotation on an ancestor win over the call's raw type.
 *
 * Per #257 the recovered annotation is now emitted STRUCTURALLY (the object's
 * member shape), not as the bare type name. A bare name is fine inside the
 * source project but becomes a dangling reference in the cross-repo `.d.ts`
 * bundle (alias lines only, no source declarations) → resolves to `any` →
 * `unverifiable`. So `RegisterRequest` is recovered as
 * `{ username: string; password: string; }`.
 *
 * The control case proves the genuinely-untyped path is preserved: an
 * untyped `await request.formData()` must stay `FormData`/`any` (faithful,
 * not a bug — #133 lists it as NOT-a-bug), never "recovered" into a
 * declared type that was never written.
 */
/** Structural form of `interface RegisterRequest { username; password }`. */
const REGISTER_REQUEST_STRUCTURAL = '{ username: string; password: string; }';
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';
const FIXTURE = path.join(FIXTURES_PATH, 'src/request-bodies.ts');
const FIXTURE_SOURCE = fs.readFileSync(FIXTURE, 'utf-8');
/** Byte span of `text` in the fixture (ASCII-only file, so byte == char offsets). */
function spanOf(text) {
    const start = FIXTURE_SOURCE.indexOf(text);
    assert.ok(start >= 0, `fixture must contain: ${text}`);
    assert.strictEqual(FIXTURE_SOURCE.indexOf(text, start + 1), -1, `fixture must contain exactly one occurrence of: ${text}`);
    const line = FIXTURE_SOURCE.slice(0, start).split('\n').length;
    return { start, end: start + text.length, line };
}
describe('Request body cast/binding regressions (#133 A1)', () => {
    let client;
    before(async () => {
        client = new SidecarClient();
        await client.start();
        await client.send({
            action: 'init',
            request_id: 'reqbody-init',
            repo_root: FIXTURES_PATH,
        });
    });
    after(async () => {
        await client.stop();
    });
    it('follows an `as T` cast on an awaited request.json()', async () => {
        // Locator covers the `request.json()` call inside
        // `(await request.json()) as RegisterRequest`. The raw type is
        // `Promise<any>`; the `as RegisterRequest` cast must win — recovered as
        // the structural member shape (#257), not the dangling name.
        const call = spanOf('(await request.json()) as RegisterRequest');
        const innerStart = call.start + '(await '.length;
        const response = await client.send({
            action: 'infer',
            request_id: 'reqbody-cast',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: call.line,
                    span_start: innerStart,
                    span_end: innerStart + 'request.json()'.length,
                    infer_kind: 'request_body',
                    alias: 'RegisterCastBody',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'RegisterCastBody');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        assert.strictEqual(inferred.type_string, REGISTER_REQUEST_STRUCTURAL, `expected the cast type to win (structural), got ${inferred.type_string}`);
        assert.strictEqual(inferred.is_explicit, true, 'a recovered declared type must be reported explicit');
    });
    it('recovers the cast type when the located node IS the `as T` cast itself', async () => {
        // Regression for the #163 Copilot finding: when the locator resolves the
        // span onto the `(await request.json()) as RegisterRequest` cast NODE
        // (not the inner call), the cast is the located node — not an ancestor —
        // so the old `extractExplicitTypeFromAncestor(located)` missed it and fell
        // back to `Promise<any>` / `any`. The fix passes the unwrapped inner node,
        // whose ancestors include the `as T`, so the cast type is recovered.
        const cast = spanOf('(await request.json()) as RegisterRequest');
        const response = await client.send({
            action: 'infer',
            request_id: 'reqbody-cast-on-node',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: cast.line,
                    span_start: cast.start,
                    span_end: cast.end,
                    infer_kind: 'request_body',
                    alias: 'RegisterCastNodeBody',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'RegisterCastNodeBody');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        assert.strictEqual(inferred.type_string, REGISTER_REQUEST_STRUCTURAL, `expected the cast type to win when located ON the cast (structural), got ${inferred.type_string}`);
        assert.strictEqual(inferred.is_explicit, true, 'a recovered declared type must be reported explicit');
    });
    it('follows a typed variable binding on an awaited request.json()', async () => {
        // `const body: RegisterRequest = await request.json();` — the typed
        // binding annotation must win over the call's `Promise<any>`.
        const stmt = spanOf('const body: RegisterRequest = await request.json();');
        const callStart = stmt.start + 'const body: RegisterRequest = await '.length;
        const response = await client.send({
            action: 'infer',
            request_id: 'reqbody-binding',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: stmt.line,
                    span_start: callStart,
                    span_end: callStart + 'request.json()'.length,
                    infer_kind: 'request_body',
                    alias: 'RegisterBindingBody',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'RegisterBindingBody');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        assert.strictEqual(inferred.type_string, REGISTER_REQUEST_STRUCTURAL, `expected the typed binding to win (structural), got ${inferred.type_string}`);
        assert.strictEqual(inferred.is_explicit, true, 'a recovered declared type must be reported explicit');
    });
    it('drills through JSON.stringify(payload) to the serialized argument type (B)', async () => {
        // Consumer `fetch` POST body is `JSON.stringify(payload)`. The call's own
        // type is `string`; the request shape must be the argument's type
        // (`RegisterRequest`), so the cross-repo edge resolves to the real payload
        // shape and not the useless `string`.
        const bodyLine = spanOf('body: JSON.stringify(payload),');
        const callStart = bodyLine.start + 'body: '.length;
        const callEnd = callStart + 'JSON.stringify(payload)'.length;
        const response = await client.send({
            action: 'infer',
            request_id: 'reqbody-stringify',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: bodyLine.line,
                    span_start: callStart,
                    span_end: callEnd,
                    infer_kind: 'request_body',
                    alias: 'StringifyBody',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'StringifyBody');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        assert.notStrictEqual(inferred.type_string, 'string', 'must drill into JSON.stringify(arg), not surface its `string` result');
        // The serialized argument is a plain identifier typed `RegisterRequest`.
        // Its own type resolves to the named interface; the bare name `RegisterRequest`
        // would dangle in the source-less cross-repo `.d.ts` bundle (alias lines only)
        // → resolve to `any` → `unverifiable`. So the resolved payload is expanded
        // STRUCTURALLY (shared `type-structural-expander.ts`), the same recovery
        // `inferResponseBody`/`inferFunctionReturn` apply, landing the real members
        // in the bundle so the consumer request shape can actually be compared.
        assert.strictEqual(inferred.type_string, REGISTER_REQUEST_STRUCTURAL, `expected the serialized argument's payload type (structural), got ${inferred.type_string}`);
    });
    it('leaves an untyped request.formData() as FormData/any (NOT-a-bug control)', async () => {
        // No cast, no typed binding → nothing to recover. Must stay faithful.
        const stmt = spanOf('const form = await request.formData();');
        const callStart = stmt.start + 'const form = await '.length;
        const response = await client.send({
            action: 'infer',
            request_id: 'reqbody-formdata',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: stmt.line,
                    span_start: callStart,
                    span_end: callStart + 'request.formData()'.length,
                    infer_kind: 'request_body',
                    alias: 'UntypedFormBody',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'UntypedFormBody');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        assert.ok(inferred.type_string === 'FormData' || inferred.type_string === 'any', `expected untyped formData to stay FormData/any, got ${inferred.type_string}`);
        assert.notStrictEqual(inferred.type_string, REGISTER_REQUEST_STRUCTURAL, 'must not invent a declared type that was never written');
    });
});
