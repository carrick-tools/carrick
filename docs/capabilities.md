# What Carrick indexes and checks

For agents. Each claim names the code on `origin/main` that makes it true.
Line numbers are from the commit this file was written on; the two marked
blocks are pinned by `tests/capability_sheet_test.rs`.

## 1. Operation kinds

Four kinds are indexed and type-checked. `OperationKey` is the enum
(`src/operation.rs:241`); the pairs are judged in `src/engine/type_compat_v2.rs`.

<!-- capability:operation-kinds -->
- http
- graphql
- socket
- pubsub
<!-- /capability:operation-kinds -->

| Kind | Key | Match |
|---|---|---|
| http | method + path | Path match. Every param syntax (`:id`, `{id}`, `[id]`, `${expr}`) collapses to `:param`, case and trailing slash ignored (`normalize_match_path`, `type_compat_v2.rs:1197`). |
| graphql | root kind (`query`, `mutation`, ...) + top-level field | Exact key (`join_identity`, `type_compat_v2.rs:1186`). |
| socket | direction (`CLIENT->SERVER`, `SERVER->CLIENT`, or `UNKNOWN`) + event name | Exact key (`type_compat_v2.rs:1182`). Namespace or channel is not part of the key (`src/operation.rs:254`). |
| pubsub | topic | Exact key. The broker is not part of the key (`type_compat_v2.rs:1190`, `src/operation.rs:263`). |

Both the producer and the consumer of a pubsub topic share one key; the
subscriber is the producer, the publisher is the consumer.

## 2. What is inferred when nothing is declared

- Request and response bodies need no annotation. The sidecar runs the
  TypeScript compiler over the service and reads the type the compiler infers
  at the call site and at the handler.
- JavaScript is read the same way, not treated as untyped: the sidecar
  compiles with `allowJs: true, checkJs: false`
  (`src/sidecar/src/project-loader.ts:81`, `src/sidecar/src/capture/index.ts:213`).
  `checkJs: false` means JS files are not reported for errors; their types are
  still inferred and emitted.

<!-- capability:js-read: yes -->

- The scanner opens files with these extensions only (`is_scanned_source`,
  `src/file_finder.rs:136`). Test paths are skipped.

<!-- capability:scanned-extensions: js jsx ts tsx -->

  So `.mjs`, `.cjs`, `.mts` and `.cts` files are never scanned (carrick#904,
  open). The workspace resolver takes the same four (`src/workspace_resolver.rs:45`).

## 3. Fact and candidate rows

Every row, and every pairing of rows, carries a source (`EdgeSource`,
`src/findings.rs:81`).

- **fact**: the source code states it outright. A pairing is a fact only when
  the producer endpoint and every consumer call site it rests on are facts.
- **candidate**: at least one row it rests on came only from the model, or the
  scan holds no row for one side of the pairing (`src/findings.rs:70`).
- A finding on a candidate row is a warning and `candidate: true`. It is
  never counted as an error, so it never makes `status` `incompatible`; it
  keeps `status` at `partially_checked` or `unresolved`
  (`carrick-cloud/lambdas/mcp-server/src/tools/check-compat.ts:385`, tool
  description of `check_compatibility`).
- A row with no stated source is read as not stated, never as candidate.

## 4. Not covered

- A transport that carries no event name, such as one `'message'` handler that
  switches on a payload field, has no key of its own. Only a narrow, guarded
  convention is read: a `JSON.parse` binding whose `type` or `event` field is
  compared to a literal inside a handler written at the registration. A
  destructured discriminator, `addEventListener("message", ...)`, and a handler
  table on a class field are not read (`src/socket_io.rs:116-145`, carrick#1298).
  The runtime's own events (`message`, `open`, `close`) are never keyed
  (`src/event_emitter.rs:122`).
- A consumer that reaches a producer through a published SDK package has no
  call site of its own. Its pairs are reported separately as `via_sdk`, with
  the verdict stored for the SDK repo's own call (`src/sdk_edges.rs:1`).
- Files outside the extension list in section 2.
- A side the scan holds no row for is not guessed; the pairing is a candidate
  or reads unverifiable.

## 5. Which MCP tool answers what

| Tool | HTTP | GraphQL / socket / pubsub |
|---|---|---|
| `check_compatibility` | verdicts | verdicts, same fields |
| `get_endpoint_types` | request and response types | by label (`QUERY`, `MUTATION`, `CLIENT->SERVER`) |
| `get_contract_pair` | both sides' types | not yet: HTTP only (`get-contract-pair.ts:20`, carrick-cloud#1136). Its response says so in `non_http_note` and points at `check_compatibility`. |
