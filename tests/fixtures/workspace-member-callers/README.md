# workspace-member-callers

A monorepo where the CALL EDGES the one scanned service records point into a
sibling workspace package (carrick#776).

`packages/app` is the only scanned service. It calls members on a client class
that `@fixture/core` publishes under its `/v2` manifest subpath — a package the
service imports by name, whose source is not in the service's own file list.
Before the fix, `call_graph` refused both halves of that: a receiver bound to an
INSTANCE resolved to nothing, and a non-relative specifier resolved to nothing.
So `get_callers` on the published member answered zero however many call sites
existed.

`@fixture/core` publishes `./v2` through its `exports` map, with a committed
`dist` beside the source: the `types` condition names a declaration file and the
default condition names build output, and the source is what the scan has to
read.

Four of the ten sites in `packages/app/src/reader.ts` record an edge. The
other six are answer keys for what must NOT resolve:

- `readUnbound` — the receiver is a cast, so the file declares no class.
- `readVendor` — the receiver's class is declared by an external package, which
  has no source in this repo.
- `readAmbiguous` — one name is bound to two different classes in one scope.
- `UntypedManager.readStreamUntyped` — the receiver is a class field the body
  never annotates, so the class states nothing about it (carrick#782).
- `readStreamByOrigin` — the receiver's origin is the package, but BOTH
  `RunClient` and `BatchClient` on its published surface declare `fetchStream`,
  so the member names no one class (carrick#781).
- `readContestedByOrigin` — a nested parameter shadows the origin, and the
  nested call site is folded into the enclosing function's.

`RunMetadataManager.readStreamThroughField` is the third resolving site: the
receiver is `this.apiClient`, a constructor parameter property, which the class
body declares as plainly as an annotated parameter does.

`readRunByOrigin` is the fourth, and the only one resolved by INFERENCE rather
than by a statement the file makes: the file names no class at all, only the
package its receiver's value came out of, and `subscribeToRun` is declared by
exactly one class across that package's surface (carrick#781).

Every `__llm__` cassette is empty, so a row exists here only because a
deterministic pass emitted it.
