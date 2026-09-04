# workspace-package-client

A monorepo where the only client the service calls is published by a sibling
workspace package and imported by PACKAGE NAME, never by a relative specifier
(carrick#666).

`packages/sdk` is the one scanned service. It imports two factories —
`@fixture/core/v2` and `@fixture/other` — asks each for a client, parks it on a
local, and calls a member on it. The client classes themselves are imported
nowhere, and `packages/core` is not part of the service's own file list, so
every route these sites reach is stated only in another package's source.

`@fixture/core` publishes its `/v2` subpath through its manifest's `exports`
map, with a committed `dist` beside the source: the `types` condition names a
declaration file and the default condition names build output, and the source
is what the scan has to read.

Every `__llm__` cassette is empty, so a row exists here only because a
deterministic pass emitted it.

What the fixture pins, beyond the one row the issue is about:

- the same member name reached through the OTHER package's factory resolves to
  the OTHER package's route,
- a receiver with no import behind it resolves to nothing,
- a name two modules of one surface declare differently resolves to nothing,
- a chained call (`client.archiveWidget(id).catch(...)`) is one row, not two.
