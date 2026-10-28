# `call-through-result-label`

Fixture for carrick#2820: call-through reads the library store's result label.
Read by `tests/call_through_result_label_test.rs`. Everything in it is
invented, including the client package `fixture-reply-client`
(`node_modules/`, committed with `git add -f`; its lockfile records the public
registry so the scan asks the store about it).

Three services:

- `packages/client` holds the wrappers, `src/users.ts`, each over a call of the
  invented library.
- `packages/cli` calls them, `src/commands.ts`.
- `services/api` serves the routes.

The store's answer is the cassette `__llm__/library-claims/default.json`: `get`
is labelled with the member `data`, `send` with the whole value, `ping` with
nothing, and the hook `useLoad` with the member `data`.

| Wrapper | Reads | Proves the body |
|---|---|---|
| `loadUser` | `reply.data` | yes: the labelled member |
| `createUser` | the call, whole | yes: labelled whole |
| `loadStatus` | `.status` | no: another member |
| `pingUsers` | `.ok` | no: the call has no label |
| `loadPage` | `reply.data.items` | no: two reads |
| `loadModel` | `toModel((await get()).data)` | no: the call returned is another call |
| `loadFrozen` | `Object.freeze(await send())` | no: the call returned is another call |
| `loadThen` | `.then((reply) => reply.data)` | no: run through a callback |
| `loadCaught` | `.catch(() => null)` | no: run through a callback |

The scan runs with site questions on, because that is what asks the store for
the labels. The rows site questions state are not in the mount graph that
call-through settles over (carrick#2795), so the test asserts what the pass
read and stored, and the unit tests in `src/call_through.rs` assert which types
the same facts keep.
