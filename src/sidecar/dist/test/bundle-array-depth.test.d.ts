/**
 * #248: a `SymbolRequest.array_depth > 0` wraps the bundled element type in that
 * many TS array levels. This is the GraphQL producer type-locate path: an SDL
 * field `orders: [Order!]!` backed by `interface Order` (with no resolver) is
 * bundled as `Order` with `array_depth: 1`, so the alias must resolve to the
 * element's fully-inlined body followed by `[]` — the shape from the element
 * type, the list depth from the SDL marker.
 *
 * Reuses UserProfile (test/fixtures/sample-repo/src/types.ts), the same nested
 * shape the explicit-symbol bundle test exercises, so we also confirm the wrap
 * does not break the structural inlining of nested named members.
 */
export {};
