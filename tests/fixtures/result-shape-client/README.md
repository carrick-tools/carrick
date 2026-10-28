# `result-shape-client`

Fixture for carrick#2819: the library store's result labels reach the scan.
`repo/` is one root service; the store's answer is the cassette
`repo/__llm__/library-claims/default.json`, and the client package
`fixture-reply-client` is invented (`repo/node_modules/`). Read by the tests
in `src/result_shapes.rs`.

| Call | Label | Expected |
|---|---|---|
| `src/client.ts:9` `api.get<User>(…)`, a reply wrapper | member `data` | verified |
| `src/client.ts:15` `api.send<User>(…)`, the caller supplies the return | whole | verified |
| `src/client.ts:20` `api.ping(…)`, a plain object | member `ok` | refused (no signature offers it) |
| `src/client.ts:25` two `api.get` calls on one line | member `data` | each keeps its span's result; the line names neither |
| `src/hook.ts:6` `useLoad(…)`, a hook returning a state union | member `data` | verified |
