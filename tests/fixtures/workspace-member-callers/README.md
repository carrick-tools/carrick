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

Two of the five sites in `packages/app/src/reader.ts` record an edge. The
other three are answer keys for what must NOT resolve:

- `readUnbound` — the receiver is a cast, so the file declares no class.
- `readVendor` — the receiver's class is declared by an external package, which
  has no source in this repo.
- `readAmbiguous` — one name is bound to two different classes in one scope.

Every `__llm__` cassette is empty, so a row exists here only because a
deterministic pass emitted it.
