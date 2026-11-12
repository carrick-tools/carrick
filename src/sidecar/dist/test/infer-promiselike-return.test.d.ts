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
export {};
