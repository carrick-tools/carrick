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
export {};
