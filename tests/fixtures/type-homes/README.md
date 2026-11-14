# `type-homes`

Fixture for carrick#2148 (H2): every type entry records where its type is
declared, whichever protocol it belongs to. Driven by `tests/type_homes_test.rs`,
which holds the answer key.

`src/models/orders.ts` declares the types; `src/models/index.ts` is a barrel
over it. The rows: an HTTP producer response imported through the barrel, a
producer request body, a consumer request body typed by a parameter, a consumer
response cast at the read, a pub/sub publisher payload, a plain-WebSocket
handler payload, and a GraphQL field whose SDL type a TypeScript type backs.
`src/client.ts` also reads a body into a type declared by a client package's
typings (`src/vendor/http-kit.d.ts`): machinery, which gets no home.
`@fixture/queue` is a stub under `node_modules`, with the library store's
answer recorded in `__llm__/library-claims`.
