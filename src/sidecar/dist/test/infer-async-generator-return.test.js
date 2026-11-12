/**
 * Stage A scaffolding for GraphQL producer type resolution: a subscription
 * resolver written as an async generator carries its response contract in the
 * YIELD position, not at the top level. Before this change `inferFunctionReturn`
 * unwrapped only `Promise<T>`, so a resolver
 *
 *     async function* f(): AsyncGenerator<Order> { ... }
 *
 * resolved to the library wrapper `AsyncGenerator<Order, …>` instead of the bare
 * `Order`. The fix adds `unwrapAsyncIterableType`, called right after the Promise
 * unwrap, so both `AsyncGenerator<T>` and `Promise<AsyncGenerator<T>>` reduce to
 * `T` before structural expansion — the named object is then expanded to its real
 * member shape, exactly as a plain `Promise<Order>` resolver already was.
 *
 * The throwaway source is written to the OS temp dir (NOT under test/fixtures),
 * so this test never touches ground-truth fixtures. The sidecar still inits
 * against the real sample-repo so the project carries a valid lib/tsconfig and
 * the global `AsyncGenerator` type resolves.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import * as fs from 'node:fs';
import * as os from 'node:os';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';
const SOURCE = `interface Order {
  id: string;
  total: number;
}

export async function* streamOrders(): AsyncGenerator<Order> {
  yield { id: 'a', total: 1 };
}

export async function* streamOrdersPromised(): Promise<AsyncGenerator<Order>> {
  return streamOrders();
}
`;
/** Byte span of \`text\` in SOURCE (ASCII-only, so byte == char offsets). */
function spanOf(text) {
    const start = SOURCE.indexOf(text);
    assert.ok(start >= 0, `source must contain: ${text}`);
    const line = SOURCE.slice(0, start).split('\n').length;
    return { start, end: start + text.length, line };
}
describe('async-generator resolver return unwrapping (GraphQL subscription scaffolding)', () => {
    let client;
    let tmpDir;
    let fixtureFile;
    before(async () => {
        tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-asyncgen-'));
        fixtureFile = path.join(tmpDir, 'stream-orders.ts');
        fs.writeFileSync(fixtureFile, SOURCE, 'utf-8');
        client = new SidecarClient();
        await client.start();
        await client.send({
            action: 'init',
            request_id: 'asyncgen-init',
            repo_root: FIXTURES_PATH,
        });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(tmpDir, { recursive: true, force: true });
    });
    it('reduces `AsyncGenerator<Order>` to the inlined member shape, not the iterator wrapper', async () => {
        const fn = spanOf('streamOrders(): AsyncGenerator<Order>');
        const response = await client.send({
            action: 'infer',
            request_id: 'asyncgen-return',
            requests: [
                {
                    file_path: fixtureFile,
                    line_number: fn.line,
                    span_start: fn.start,
                    span_end: fn.end,
                    infer_kind: 'function_return',
                    alias: 'StreamOrdersReturn',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'StreamOrdersReturn');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        // The bug: the iterator wrapper leaking into the contract.
        assert.ok(!/AsyncGenerator/.test(inferred.type_string), `must peel the async-iterator wrapper, got: ${inferred.type_string}`);
        // The fix: the yield type's real member shape (structurally expanded, like
        // a plain Promise<Order> resolver).
        assert.strictEqual(inferred.type_string, '{ id: string; total: number; }', `expected the inlined yield shape, got: ${inferred.type_string}`);
        // The anchor is the yield type's source symbol, never the wrapper.
        assert.strictEqual(inferred.primary_type_symbol, 'Order', `expected primary_type_symbol "Order", got: ${inferred.primary_type_symbol}`);
    });
    it('peels `Promise<AsyncGenerator<Order>>` through both layers to the yield shape', async () => {
        const fn = spanOf('streamOrdersPromised(): Promise<AsyncGenerator<Order>>');
        const response = await client.send({
            action: 'infer',
            request_id: 'asyncgen-promised-return',
            requests: [
                {
                    file_path: fixtureFile,
                    line_number: fn.line,
                    span_start: fn.start,
                    span_end: fn.end,
                    infer_kind: 'function_return',
                    alias: 'StreamOrdersPromisedReturn',
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === 'StreamOrdersPromisedReturn');
        assert.ok(inferred, `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`);
        assert.ok(!/AsyncGenerator|Promise/.test(inferred.type_string), `must peel both Promise and async-iterator wrappers, got: ${inferred.type_string}`);
        assert.strictEqual(inferred.type_string, '{ id: string; total: number; }', `expected the inlined yield shape, got: ${inferred.type_string}`);
        assert.strictEqual(inferred.primary_type_symbol, 'Order');
    });
});
