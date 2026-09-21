# Module resolution for call edges

How the scanner decides which file an import specifier names when it builds
`function_definitions[].calls`, the edges `get_callers` inverts. Code:
`src/workspace_resolver.rs` (probing and precedence) and `src/module_aliases.rs`
(what config states). Introduced by carrick#1104.

## Order

For a specifier written in a file, first match wins:

1. **Relative** (`./`, `../`): resolved against the importing file. Unchanged
   by alias support.
2. **Deno import maps.** Every `deno.json`/`deno.jsonc` in the tree, plus the
   `importMap` file one names. A map applies to files under its config's
   directory. The most specific scope is tried first, and a `scopes` block is
   tried before `imports`. An exact key beats a prefix key (`"@/": "./src/"`),
   and the longest prefix wins. Targets resolve against the declaring file. A
   key that maps to `npm:`, `jsr:` or a URL decides the lookup and names no
   file.
3. **package.json `imports`**, for a `#` specifier only. The importer's nearest
   package.json decides alone, as in Node. It supports conditions and one `*`.
   A condition leaf that names TypeScript source beats build output, and a
   declaration file is never chosen.
4. **tsconfig / jsconfig `paths`, then `baseUrl`.** The governing config is the
   service's `carrick.json` `tsconfig` for files under the service directory.
   For any other file it is the nearest `tsconfig.json` (or else
   `jsconfig.json`) above the file. `extends` is followed (string or array,
   relative, workspace package, or `node_modules`). Within that chain the
   nearest layer that sets `paths` supplies the whole map, and `baseUrl`
   resolves against the config that declares it. `paths` targets resolve
   against `baseUrl` when one is set, otherwise against the directory of the
   config that declared `paths`. An exact pattern beats a wildcard, and the
   longest prefix wins. If every target misses, `baseUrl` is tried.
5. **Workspace package names**, through the member's `exports` (wildcard keys
   expanded) or `main`.
6. **Declared external packages**: an import, never an edge.

Every candidate goes through one probe: the path as written, then with a source
extension added, then `.js` swapped for `.ts`, then an `index` file.

## Why a config reader and not the compiler

The type sidecar already runs `ts.resolveModuleName` and reads Deno's module
graph, so asking it would avoid a second implementation. It is not used for
call edges, for these reasons:

- **Availability.** The sidecar is optional. A scan with
  `CARRICK_ALLOW_MISSING_TYPES`, a scan with no Node, or a sidecar that fails
  to start (carrick#748) still records call edges. With resolution in the
  sidecar, the call graph would differ between two machines scanning the same
  commit.
- **Speed.** Reading a few config files takes milliseconds. A sidecar round
  trip per service, plus `deno info` over the whole graph, is too slow for the
  edit-time re-check.
- **Scope.** The compiler only resolves files its program includes. The call
  graph also reads sibling packages on demand.

The compiler is the test oracle instead:
`tests/module_resolution_parity_test.rs` resolves every case with
`ts.resolveModuleName` and with this resolver, and fails on any disagreement.
Deno maps are pinned by the Deno fixture in `tests/alias_import_callers_test.rs`.

## CommonJS

A module states the same two facts in two grammars, and the call graph reads
both (`src/commonjs.rs`, carrick#1348). Everything above — the specifier order,
the probe, the re-export hops — is unchanged: a `require` specifier resolves
exactly as the same string in an `import` would.

Read on the BINDING side, at module scope only:

| Written | Read as |
|---|---|
| `const { x } = require("./m")` | a named import of `x` |
| `const { a: b } = require("./m")` | a named import of `a` bound to `b` |
| `const m = require("./m")` | a namespace import: `m.x()` is `./m`'s own `x` |
| `const x = require("./m").x` | a named import of `x` |
| `import x = require("./m")` | a namespace import |

Read on the EXPORT side: `module.exports = { x }`, `module.exports = { a: impl }`,
`module.exports = name`, `module.exports.x = …`, `exports.x = …`, and
`module.exports = require("./other")`, which republishes another module's table
the way `export * from` does. A name published both ways keeps its ESM meaning.

Two forms are not read, and each has a reason rather than an omission:

- **A computed specifier** (`require(name)`): nothing in the source says which
  module it loads. The binding is not recorded, and the count is stated at
  `info` with what to write instead.
- **A require inside a function body**: the per-file table has one entry per
  name, and two functions may bind one name to different modules. Reading them
  by name alone would let one answer for the other, which is a wrong edge
  rather than a missing one (carrick#1352). The lexical receiver walk keys by
  scope, so a member call on such a binding does resolve.
- **A whole-module binding that is CALLED** (`const fn = require("./m"); fn()`)
  is looked up as `./m`'s own `fn`, so it resolves where the binding name
  matches a definition in that module — the usual case. A module that publishes
  its function under a different name (`module.exports = otherName`) records no
  edge for it.

The four passes that build ANALYZER inputs — mount, wrapper, SDK surface,
GraphQL — do not read CommonJS exports, for the reason the section below gives
for aliases: a module they newly resolve changes what the model is asked.
`BindingResolver::reading_commonjs` is taken by the call graph alone, and
carrick#1353 removes the split.

## What is not read, and how it is reported

- **Aliases defined in code**: a bundler's `resolve.alias`, a Babel
  module-resolver option, a framework config file. These are not config, and
  reading them would mean evaluating code. The call graph counts every import a
  call depended on that resolved to no file. It splits the count with the npm
  package-name grammar:
  - A specifier no package can be named by (`~/x`, `@/x`, `#x`, `$lib/x`) is
    logged at `info` with its import count and the first specifiers.
  - A package-shaped specifier that no manifest declares (a builtin written
    without `node:`, or an undeclared dependency) is counted at `debug`.
- **A config mapping whose target is not on disk** is a different fact from
  both of those, and has its own outcome
  (`Resolution::AliasTargetMissing`) and its own `info` line naming the
  count, the mapping as written, the path the MAPPING names (its target with
  the `*` removed, shared by every specifier under the key — not the file one
  import wanted) and what to do about it (carrick#1273). The repo STATED where the module lives, so the
  miss is a tree that was not fully prepared — most often a generated
  directory whose generator has not run — rather than a specifier nothing
  accounts for. Whether the target is there at all is a `stat`, and it
  decides which instruction the line ends on. A mapping that
  misses does not end the lookup: it can shadow a package that is also
  installed, so the claim is carried down and stated only where the lookup
  gives up for good. `baseUrl` declares no key and makes no such claim, so a
  miss under it alone stays unresolved.
- **An `extends` that names no config on disk** (a package not installed) is
  logged at `info`. Aliases it would have supplied are not read.
- **A nested Deno config outside a workspace** is still scoped to its
  directory, and a root map also applies beneath it. Deno would use only the
  nearest config there.

These are logged, not recorded in `scan_health`. They repeat on every scan of
the same tree, and `scan_health` counts losses the scan could not account for.

## Analyzer inputs stay on the manifest-only index

`WorkspaceIndex::build` reads no aliases, and `BindingResolver::new` follows
relative hops only. The wrapper-context and mount passes build analyzer inputs
from them. Resolving more specifiers there would change what the model is
asked, and cached answers would stop replaying. Moving the analyzer path onto
the same resolver is carrick#474.

Three surfaces read the alias-bearing index, and none of them is an analyzer
input: the call graph's edges (carrick#1104), the GraphQL hop that follows a
field's named resolver (carrick#1411), and every type request the sidecar is
sent — endpoint and data-call types, socket and pub/sub payloads, GraphQL
consumer anchors, and the manifest's `TypeHome` stamp (carrick#1416). The
last of these is built once per service as `engine::service_module_index` and
passed down; it is what turns the specifier a type was located through into
the file the sidecar reads.

Call edges do feed one model input: a function's intent context includes the
intents of the functions it calls (`intent_generator::compute_intent_hash`).
When an alias import gains an edge, the caller's intent regenerates once on the
next scan, and so do the callers of any function whose intent text changes.
