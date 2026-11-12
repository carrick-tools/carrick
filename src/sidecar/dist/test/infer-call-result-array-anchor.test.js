/**
 * #336: a `call_result` inference that resolves to an array type must anchor
 * on the ELEMENT symbol and report the peeled depth, exactly as the producer
 * paths do since #306. The live repro is `axios.get<Order[]>(...)`: the LLM
 * anchor is the bare element (`Order`), so the explicit bundle pre-claims the
 * consumer alias, and without a depth reported here `apply_inferred_array_depth`
 * (Rust) has nothing to copy onto the `SymbolRequest` and the bundler renders
 * `export interface <alias> {...}` with the `[]` gone. The producer keeps
 * `Order[]`, so every scan reports a false array-vs-scalar contract risk.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';
const FIXTURE = path.join(FIXTURES_PATH, 'src/wrapper-usage.ts');
const OPAQUE_FIXTURE = path.join(FIXTURES_PATH, 'src/wrapper-opaque.ts');
const MULTILINE_FIXTURE = path.join(FIXTURES_PATH, 'src/wrapper-multiline.ts');
/** Byte span of `text` in the fixture (ASCII-only file, so byte == char offsets). */
function spanOf(text, fixturePath = FIXTURE) {
    const source = fs.readFileSync(fixturePath, 'utf-8');
    const start = source.indexOf(text);
    assert.ok(start >= 0, `fixture must contain: ${text}`);
    assert.strictEqual(source.indexOf(text, start + 1), -1, `fixture must contain exactly one occurrence of: ${text}`);
    const line = source.slice(0, start).split('\n').length;
    return { start, end: start + text.length, line };
}
describe('call_result array payloads anchor on the element symbol with array_depth (#336)', () => {
    let client;
    before(async () => {
        client = new SidecarClient();
        await client.start();
        await client.send({
            action: 'init',
            request_id: 'call-arr-init',
            repo_root: FIXTURES_PATH,
        });
    });
    after(async () => {
        await client.stop();
    });
    it('explicit generic `apiGet<UserData[]>` unwrapped by a rule → anchor UserData, depth 1', async () => {
        const call = spanOf("apiGet<UserData[]>('/api/users')");
        const response = await client.send({
            action: 'infer',
            request_id: 'call-arr-wrapped',
            extraction_config: {
                rules: [
                    {
                        wrapperSymbols: ['ApiResponse'],
                        originModuleGlobs: ['wrapper-lib', 'wrapper-lib/*'],
                        payloadGenericIndex: 0,
                    },
                ],
            },
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: call.line,
                    span_start: call.start,
                    span_end: call.end,
                    infer_kind: 'call_result',
                    alias: 'UserArrayConsumer',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'UserArrayConsumer');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        assert.strictEqual(inferred.primary_type_symbol, 'UserData', `anchor must be the element symbol, got ${JSON.stringify(inferred.primary_type_symbol)}`);
        assert.strictEqual(inferred.array_depth, 1, 'without the depth the explicit bundle for the same alias erases the []');
    });
    it('unwrapped `Promise<UserData[]>` call → anchor UserData, depth 1', async () => {
        const call = spanOf('await fetchUsersDirect()');
        const callStart = call.start + 'await '.length;
        const response = await client.send({
            action: 'infer',
            request_id: 'call-arr-direct',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: call.line,
                    span_start: callStart,
                    span_end: call.end,
                    infer_kind: 'call_result',
                    alias: 'UserArrayDirectConsumer',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'UserArrayDirectConsumer');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        assert.strictEqual(inferred.primary_type_symbol, 'UserData');
        assert.strictEqual(inferred.array_depth, 1);
    });
    it('recursive unwrap that collapses to `unknown` must not anchor on a wrapper symbol', async () => {
        const call = spanOf("apiGetOpaque('/api/opaque')", OPAQUE_FIXTURE);
        const response = await client.send({
            action: 'infer',
            request_id: 'call-arr-opaque',
            extraction_config: {
                rules: [
                    {
                        wrapperSymbols: ['ApiResponse'],
                        originModuleGlobs: ['wrapper-lib', 'wrapper-lib/*'],
                        payloadGenericIndex: 0,
                        unwrapRecursively: true,
                    },
                    {
                        // Verifies OpaqueHandle's identity but can extract nothing (no
                        // generics, no payloadPropertyPath), so the recursive inner pass
                        // collapses to the unresolved `unknown` sentinel.
                        wrapperSymbols: ['OpaqueHandle'],
                        originModuleGlobs: ['wrapper-lib', 'wrapper-lib/*'],
                    },
                ],
            },
            requests: [
                {
                    file_path: OPAQUE_FIXTURE,
                    line_number: call.line,
                    span_start: call.start,
                    span_end: call.end,
                    infer_kind: 'call_result',
                    alias: 'OpaqueConsumer',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'OpaqueConsumer');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        assert.strictEqual(inferred.type_string, 'unknown', 'verified machinery with no recoverable payload collapses to unknown');
        assert.strictEqual(inferred.primary_type_symbol, undefined, `an unresolved unwrap must not anchor on the collapsed wrapper, got ${JSON.stringify(inferred.primary_type_symbol)}`);
        assert.strictEqual(inferred.array_depth, undefined, `an unresolved unwrap must not report an array_depth, got ${inferred.array_depth}`);
    });
    it('scalar wrapper payload keeps its anchor and reports NO array_depth', async () => {
        const call = spanOf('client.fetchUser();\n  return resp.data');
        const response = await client.send({
            action: 'infer',
            request_id: 'call-arr-scalar',
            extraction_config: {
                rules: [
                    {
                        wrapperSymbols: ['ApiResponse'],
                        originModuleGlobs: ['wrapper-lib', 'wrapper-lib/*'],
                        payloadGenericIndex: 0,
                    },
                ],
            },
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: call.line,
                    span_start: call.start,
                    span_end: call.start + 'client.fetchUser()'.length,
                    infer_kind: 'call_result',
                    alias: 'UserScalarConsumer',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'UserScalarConsumer');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        assert.strictEqual(inferred.primary_type_symbol, 'UserData');
        assert.strictEqual(inferred.array_depth, undefined, `a scalar must not report an array_depth, got ${inferred.array_depth}`);
    });
    // #336 reopened: the live repro is a MULTI-LINE `axios.get<Order[]>(` call
    // whose binding is last used as a scalar projection (`.data.length`). Both
    // halves of that shape used to lose the anchor:
    //  - the LLM's compact single-line locator failed exact matching (the
    //    normalized node text keeps a space after `(`), so the fallback bound a
    //    fragment inside the call instead of the call, and
    //  - even with the call found, the terminal-use walk anchored on `number`
    //    (no symbol, no depth) instead of the call's own payload.
    const MULTILINE_RULES = [
        {
            wrapperSymbols: ['ApiResponse'],
            originModuleGlobs: ['wrapper-lib', 'wrapper-lib/*'],
            payloadGenericIndex: 0,
        },
    ];
    it('multi-line call located by the single-line LLM print → anchor OrderData, depth 1 (#336 live shape)', async () => {
        const callStart = spanOf('apiGetOrders<OrderData[]>(\n    `${ORDER_SERVICE_URL}/api/orders`,', MULTILINE_FIXTURE);
        const response = await client.send({
            action: 'infer',
            request_id: 'call-arr-multiline-text',
            extraction_config: { rules: MULTILINE_RULES },
            requests: [
                {
                    file_path: MULTILINE_FIXTURE,
                    line_number: callStart.line,
                    infer_kind: 'call_result',
                    // The LLM prints the call compactly on one line; the source spreads
                    // it over three lines with a trailing comma.
                    expression_text: 'apiGetOrders<OrderData[]>(`${ORDER_SERVICE_URL}/api/orders`)',
                    expression_line: callStart.line,
                    alias: 'OrderArrayMultilineConsumer',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'OrderArrayMultilineConsumer');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        assert.strictEqual(inferred.primary_type_symbol, 'OrderData', `anchor must be the call payload's element symbol, got ${JSON.stringify(inferred.primary_type_symbol)} (type_string: ${JSON.stringify(inferred.type_string)})`);
        assert.strictEqual(inferred.array_depth, 1, 'without the depth the explicit bundle renders the bare element and the [] is erased');
    });
    it('terminal scalar use (`.data.length`) must not erase the call payload anchor (span locator)', async () => {
        const source = fs.readFileSync(MULTILINE_FIXTURE, 'utf-8');
        const callText = 'apiGetOrders<OrderData[]>(\n    `${ORDER_SERVICE_URL}/api/orders`,\n  )';
        const start = source.indexOf(callText);
        assert.ok(start >= 0, 'fixture must contain the multi-line call');
        const end = start + callText.length;
        const line = source.slice(0, start).split('\n').length;
        const response = await client.send({
            action: 'infer',
            request_id: 'call-arr-multiline-span',
            extraction_config: { rules: MULTILINE_RULES },
            requests: [
                {
                    file_path: MULTILINE_FIXTURE,
                    line_number: line,
                    span_start: start,
                    span_end: end,
                    infer_kind: 'call_result',
                    alias: 'OrderArraySpanConsumer',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'OrderArraySpanConsumer');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        assert.strictEqual(inferred.primary_type_symbol, 'OrderData', `anchor must come from the call's payload, not the terminal use, got ${JSON.stringify(inferred.primary_type_symbol)} (type_string: ${JSON.stringify(inferred.type_string)})`);
        assert.strictEqual(inferred.array_depth, 1);
    });
    it('SWC-shaped span (1-based) inside a route registration binds the payload call, not the registration (#336 third path)', async () => {
        // The scanner sends raw SWC BytePos spans: 1-based, so both ends sit one
        // byte past the ts-morph 0-based offsets. Strict containment excluded the
        // real call (the shifted end overshoots its end by one byte) and bound the
        // enclosing `fakeRouter.get(...)` registration instead, anchoring the
        // router type with no depth — so the explicit bundle rendered the bare
        // element interface and the [] was erased.
        const source = fs.readFileSync(MULTILINE_FIXTURE, 'utf-8');
        const callText = 'apiGetOrders<OrderData[]>(\n    `${ORDER_SERVICE_URL}/api/orders-status`,\n  )';
        const start = source.indexOf(callText);
        assert.ok(start >= 0, 'fixture must contain the registration-wrapped call');
        const end = start + callText.length;
        const line = source.slice(0, start).split('\n').length;
        const response = await client.send({
            action: 'infer',
            request_id: 'call-arr-swc-span',
            extraction_config: { rules: MULTILINE_RULES },
            requests: [
                {
                    file_path: MULTILINE_FIXTURE,
                    line_number: line,
                    // SWC convention: BytePos(1) is the first byte of the file.
                    span_start: start + 1,
                    span_end: end + 1,
                    infer_kind: 'call_result',
                    alias: 'OrderArraySwcSpanConsumer',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'OrderArraySwcSpanConsumer');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        assert.strictEqual(inferred.primary_type_symbol, 'OrderData', `span lookup must bind the payload call, not the enclosing registration, got ${JSON.stringify(inferred.primary_type_symbol)} (type_string: ${JSON.stringify(inferred.type_string)})`);
        assert.strictEqual(inferred.array_depth, 1, 'without the depth the explicit bundle renders the bare element and the [] is erased');
    });
    it('untyped client (`any`, no node_modules in CI) still anchors from the explicit call generic (#336 CI shape)', async () => {
        // The live CI scan runs against a checkout with NO node_modules, so
        // `axios` is `any`, the call's semantic type is `any`, and the semantic
        // anchor path finds no symbol — the depth was erased even with the span
        // and unwrap fixes in place. The single explicit generic on the call is
        // the caller's payload claim and must anchor instead.
        const source = fs.readFileSync(MULTILINE_FIXTURE, 'utf-8');
        const callText = 'untypedAxios.get<OrderData[]>(\n    `${ORDER_SERVICE_URL}/api/orders-untyped`,\n  )';
        const start = source.indexOf(callText);
        assert.ok(start >= 0, 'fixture must contain the untyped-client call');
        const end = start + callText.length;
        const line = source.slice(0, start).split('\n').length;
        const response = await client.send({
            action: 'infer',
            request_id: 'call-arr-untyped-client',
            // Rules are present (the cloud generates them either way) but inert:
            // they cannot match a wrapper on an `any` call type, exactly as in CI.
            extraction_config: { rules: MULTILINE_RULES },
            requests: [
                {
                    file_path: MULTILINE_FIXTURE,
                    line_number: line,
                    // SWC convention, as the scanner sends it.
                    span_start: start + 1,
                    span_end: end + 1,
                    infer_kind: 'call_result',
                    alias: 'OrderArrayUntypedClientConsumer',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'OrderArrayUntypedClientConsumer');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        assert.strictEqual(inferred.primary_type_symbol, 'OrderData', `an untyped client must anchor from the explicit call generic, got ${JSON.stringify(inferred.primary_type_symbol)} (type_string: ${JSON.stringify(inferred.type_string)})`);
        assert.strictEqual(inferred.array_depth, 1, 'without the depth the explicit bundle renders the bare element and the [] is erased');
    });
});
