/**
 * Regression for carrick#768: a route that states its contract with
 * `satisfies X` lost the NAME `X` once the repo's dependencies were installed.
 *
 * The anchor (`primary_type_symbol`) was read off the RESOLVED type of the
 * annotation — `getSymbol() || getAliasSymbol()`. That works for an interface,
 * whose resolved type carries its own symbol. It does not work for the shape
 * most schema-first codebases write:
 *
 *     export type OrderBody = Inferred<typeof OrderSchema>;
 *
 * The alias resolves to an INSTANTIATED type. TypeScript keeps no alias symbol
 * on it, so `getSymbol()` answers the synthetic `__type` and the anchor came
 * back null — the route printed a correct shape that no reader could trace to
 * an importable name.
 *
 * On a bare checkout the same route DID report the name, for the wrong reason:
 * the annotated type did not resolve at all, so the unresolved reference's own
 * identifier survived as the symbol. Installing the dependencies is what
 * "lost" it. The declaration file is inside the repo either way (TypeScript
 * realpaths a workspace symlink), so nothing here is about `node_modules`.
 *
 * The fix reads the anchor from the ANNOTATION AS WRITTEN: the type node names
 * a type, and that name is exactly "the type a consumer imports", whether or
 * not the compiler kept an alias symbol on what it resolves to.
 *
 * Deliberate limits, one negative case each:
 *  - a generic instantiation (`satisfies Envelope<Order>`) names the wrapper,
 *    not the payload, so it anchors nothing;
 *  - a lib global (`satisfies Record<string, number>`) is not an importable
 *    contract;
 *  - an inline object annotation (`satisfies { id: string }`) names nothing.
 */
export {};
