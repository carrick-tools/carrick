/**
 * Generalization regression for `unwrapPromiseType`: the awaited-type unwrap
 * used to gate on the literal `Promise` symbol only, so a function returning
 * `PromiseLike<T>` (or `Awaited<...>`) leaked the wrapper into the contract.
 *
 * The fix resolves the awaited type structurally on the symbol/alias name,
 * accepting `Promise`, `PromiseLike` and the `Awaited<...>` utility type while
 * leaving any NON-thenable type (e.g. a plain object, or `AsyncGenerator<T>`)
 * untouched. This test pins:
 *   - `(): Promise<User>`     → inlined `User` member shape (existing behavior);
 *   - `(): PromiseLike<User>` → inlined `User` member shape (the new case);
 *   - a non-promise return    → unchanged (no over-unwrapping);
 *   - the corpus shape `Promise<ApiResponse<Order>>` → `ApiResponse<Order>`
 *     (then structurally expanded downstream to `{ data; errors }`), confirming
 *     no regression on the existing HTTP-resolution output.
 *
 * The throwaway source is written to the OS temp dir (NOT under test/fixtures),
 * so this test never touches ground-truth fixtures. The sidecar inits against
 * the real sample-repo so the project carries a valid lib/tsconfig and the
 * global `Promise`/`PromiseLike` types resolve.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import * as fs from 'node:fs';
import * as os from 'node:os';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';
const SOURCE = `interface User {
  id: string;
  name: string;
}

interface Order {
  id: string;
  total: number;
}

interface ApiResponse<T> {
  data: T;
  errors: string[];
}

export function getUserPromise(): Promise<User> {
  return Promise.resolve({ id: 'a', name: 'b' });
}

export function getUserPromiseLike(): PromiseLike<User> {
  return Promise.resolve({ id: 'a', name: 'b' });
}

export function getOrderEnvelope(): Promise<ApiResponse<Order>> {
  return Promise.resolve({ data: { id: 'o', total: 1 }, errors: [] });
}

export function getUserSync(): User {
  return { id: 'a', name: 'b' };
}
`;
/** Byte span of `text` in SOURCE (ASCII-only, so byte == char offsets). */
function spanOf(text) {
    const start = SOURCE.indexOf(text);
    assert.ok(start >= 0, `source must contain: ${text}`);
    const line = SOURCE.slice(0, start).split('\n').length;
    return { start, end: start + text.length, line };
}
describe('awaited-type unwrap generalizes to PromiseLike / Awaited', () => {
    let client;
    let tmpDir;
    let fixtureFile;
    before(async () => {
        tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-promiselike-'));
        fixtureFile = path.join(tmpDir, 'promise-returns.ts');
        fs.writeFileSync(fixtureFile, SOURCE, 'utf-8');
        client = new SidecarClient();
        await client.start();
        await client.send({
            action: 'init',
            request_id: 'promiselike-init',
            repo_root: FIXTURES_PATH,
        });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(tmpDir, { recursive: true, force: true });
    });
    async function inferReturn(signature, alias) {
        const fn = spanOf(signature);
        const response = await client.send({
            action: 'infer',
            request_id: `promiselike-${alias}`,
            requests: [
                {
                    file_path: fixtureFile,
                    line_number: fn.line,
                    span_start: fn.start,
                    span_end: fn.end,
                    infer_kind: 'function_return',
                    alias,
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === alias);
        assert.ok(inferred, `expected an inferred type for ${alias}, got errors: ${JSON.stringify(response.errors)}`);
        return inferred;
    }
    it('inlines `Promise<User>` to the User member shape', async () => {
        const inferred = await inferReturn('getUserPromise(): Promise<User>', 'PromiseUser');
        assert.ok(!/Promise/.test(inferred.type_string), `must peel the Promise wrapper, got: ${inferred.type_string}`);
        assert.strictEqual(inferred.type_string, '{ id: string; name: string; }');
        assert.strictEqual(inferred.primary_type_symbol, 'User');
    });
    it('inlines `PromiseLike<User>` to the User member shape (the new case)', async () => {
        const inferred = await inferReturn('getUserPromiseLike(): PromiseLike<User>', 'PromiseLikeUser');
        assert.ok(!/PromiseLike|Promise/.test(inferred.type_string), `must peel the PromiseLike wrapper, got: ${inferred.type_string}`);
        assert.strictEqual(inferred.type_string, '{ id: string; name: string; }');
        assert.strictEqual(inferred.primary_type_symbol, 'User');
    });
    it('reduces the corpus shape `Promise<ApiResponse<Order>>` to ApiResponse<Order> and expands it', async () => {
        const inferred = await inferReturn('getOrderEnvelope(): Promise<ApiResponse<Order>>', 'OrderEnvelope');
        // No Promise wrapper survives; the envelope is structurally expanded to its
        // members (the unchanged downstream behavior), and Order is inlined within.
        assert.ok(!/Promise/.test(inferred.type_string), `must peel the Promise wrapper, got: ${inferred.type_string}`);
        assert.match(inferred.type_string, /data:/, `expected the ApiResponse envelope members, got: ${inferred.type_string}`);
        assert.match(inferred.type_string, /errors:/, `expected the ApiResponse envelope members, got: ${inferred.type_string}`);
        assert.strictEqual(inferred.primary_type_symbol, 'ApiResponse');
    });
    it('leaves a non-promise return unchanged (no over-unwrapping)', async () => {
        const inferred = await inferReturn('getUserSync(): User', 'SyncUser');
        // A plain object return inlines to its member shape, exactly as before — the
        // awaited unwrap is a no-op on a non-thenable type.
        assert.strictEqual(inferred.type_string, '{ id: string; name: string; }');
        assert.strictEqual(inferred.primary_type_symbol, 'User');
    });
});
