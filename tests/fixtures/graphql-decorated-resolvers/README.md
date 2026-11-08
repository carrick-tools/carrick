# `graphql-decorated-resolvers`

Fixture for carrick#3043 (SB1): a GraphQL root field written as a method
decorator. `tests/graphql_decorated_resolvers_test.rs` runs it.

Everything in it is invented. Three packages (public registry,
`package-lock.json`; declarations hand-written in the vendored
`node_modules`), and the store's answer is the cassette
`__llm__/library-claims/default.json`:

- `@fixture/gql-decorators` 2.0.0: `Query`, `Mutation` and `Subscription`
  (role `server_framework`, one `receive` op each in the `decorate` form,
  protocol `graphql`, `method` the root type, name
  `first: [arg 0, arg 1 key name, decorated]`); `ResolveField` and `Resolver`
  state nothing.
- `@fixture/gql-options` 1.0.0: `Query(options?)`, name
  `first: [arg 0 key name, decorated]`.
- `@fixture/http-params` 1.0.0: `Query(key)` is a parameter decorator,
  unlabelled.

Three services. Answer key:

| Site | Row |
|---|---|
| `backend/src/resolvers.ts:14` | endpoint `query me` (`@Query(() => Object)`: the method's name) |
| `backend/src/resolvers.ts:19` | endpoint `mutation renameThing` (the `name` option) |
| `backend/src/resolvers.ts:24` | endpoint `query byString` (argument 0 outranks the method) |
| `backend/src/resolvers.ts:29` | endpoint `subscription events` |
| `backend/src/resolvers.ts:34` | endpoint `query everything` (`@Query()`) |
| `backend/src/resolvers.ts:39` | endpoint `query described` (an unrelated option does not name it) |
| `backend/src/resolvers.ts:44` | endpoint `query viaOptions` (the second package) |
| `backend/src/resolvers.ts:49` | endpoint `query plain` |
| `backend/src/resolvers.ts:54` | nothing: the `name` option is not a literal |
| `backend/src/resolvers.ts:59` | nothing: a field resolver is no root operation |
| `backend/src/resolvers.ts:65` | nothing: a parameter decorator is no site |
| `consumer/src/documents.ts` | one call per root field of the six documents, same keys |
| `printed/schema.graphql:2` | endpoint `query printedField` (the printed schema's row, written once) |
| `printed/src/resolvers.ts:10` | endpoint `query onlyDecorated` |
