/**
 * Regression: the explicit-symbol bundle path (`bundle` action →
 * TypeBundler.extractTypeDefinition) must inline NESTED named members, not just
 * the top-level shape.
 *
 * This is the path the GraphQL/Socket consumer anchors take (see
 * file_orchestrator.rs `collect_graphql_type_requests` /
 * `collect_socket_type_requests`): a `SymbolRequest` for the consumer's TS
 * result type is bundled, then `resolve_definitions` expands it. Socket payloads
 * have flat primitive fields, so the gap stayed hidden until a symbol with a
 * nested named member appeared (GraphQL `OrderView { total: MoneyView }`).
 *
 * The bundler used to emit the interface's verbatim source text, so a nested
 * named member (`settings: UserSettings`) stayed a bare reference. The bundle
 * carries only the alias line — never `UserSettings`'s declaration — so that
 * reference is dangling and resolves to `any` downstream, and the later
 * structural expand has nothing to inline against. The fix expands the body
 * structurally at bundle time, in the source project where the nested symbol
 * still resolves.
 *
 * UserProfile (test/fixtures/sample-repo/src/types.ts) mirrors the OrderView
 * shape: an `extends`-inherited base, a nested named member (`UserSettings`),
 * optional fields (`bio?`/`avatar?`), a builtin (`Date`) that must stay by name,
 * and a string-literal union.
 */
export {};
