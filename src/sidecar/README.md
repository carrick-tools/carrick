# TypeSidecar - Compiler-Based Type Extraction

The TypeSidecar is a warm-standby Node.js process that gives the Carrick analysis engine real TypeScript type resolution. It drives the compiler through `ts-morph`, so a type is whatever the compiler says it is rather than whatever a regex could reach.

## Overview

### Why a Sidecar?

1. **Accuracy**: type resolution through the compiler, not UTF-16 position arithmetic
2. **Inference**: a type at a location, whether or not the author annotated it
3. **Parallel startup**: spawned at CLI start; the SWC scan proceeds while it initialises
4. **Warm standby**: the process, and the program it built, survive between requests

### Capabilities

- **Capture**: emit a per-service declaration stub package with the compiler's own `.d.ts` emit (`capture_v2`)
- **Judge**: typecheck generated probes over those stubs in a scratch workspace, one verdict per matched pair (`check_v2`)
- **Inference**: resolve the type at a file/line locator, unwrapping framework machinery by the caller's rules (`infer`)
- **Definition resolution**: the as-written and fully-inlined structural form of a captured alias (`resolve_definitions`)

## Building

```bash
cd src/sidecar
npm install
npm ci --prefix lister   # the bundler the lister artifact test builds with
npm run build   # tsc, output in dist/
npm test        # node --test over dist/test
```

`npm test` runs the COMPILED tests, so a stale `dist/` makes the suite lie. The Rust integration tests spawn `dist/src/index.js` for the same reason; the pre-commit hook rebuilds it.

### The surface lister artifact

```bash
npm run build
npm ci --prefix lister
node lister/build.mjs --tag <release tag> --source-sha <commit>   # default --out dist/lister
```

Bundles `dist/src/index.js` into one self-contained ESM file, `carrick-lister.mjs`, with every dependency inlined, the TypeScript default library included. Beside it, `carrick-lister.manifest.json` holds `tag`, `source_sha`, `entry`, `node_floor` (`22`), `protocol` (`sidecar-stdio`, this README's protocol) and `files` (each file's sha256). Every release attaches both files (`.github/workflows/wasm-artifact.yml`), and carrick-cloud's library store pins them to run `list_library_surface` on packages as published (carrick#1660). The artifact runs on Node 22, the store's runtime; `test/lister/lister-artifact.test.ts` checks that it reads nothing outside its own file and the package directory it is given. The bundler has its own manifest in `lister/`, so the sidecar's install, which every Action run repeats, does not carry it.

## Usage

### From Rust

The sidecar is managed by `TypeSidecar` in `src/services/type_sidecar.rs`:

```rust
use crate::services::type_sidecar::TypeSidecar;

// Spawn at CLI startup and start init in the background (non-blocking)
let sidecar = TypeSidecar::spawn(&sidecar_path)?;
sidecar.start_init(&absolute_repo_path, None);

// Requests block until the sidecar is ready, then until they answer
let result = sidecar.resolve_all_types(&explicit_symbols, &infer_requests, None)?;
let captured = sidecar.capture_v2(&repo_root, service_name, &anchors, &out_dir, None)?;
```

### Standalone

```bash
node dist/src/index.js
```

Then write one JSON request per line on stdin. Every example below is a real request: they are validated against `src/validators.ts`, which is the authority on the shape.

## Message Protocol

JSON over stdio:
- **stdin**: one JSON request per line
- **stdout**: one JSON response per line, and nothing else
- **stderr**: logs

Every request carries `request_id` and `action`. Every response echoes `request_id` and carries `status`.

### Which actions need a project

`init` resolves a project; `bundle`, `emit_surface`, `infer`, `resolve_definitions`, `retype_check`, `verify_client_semantics`, `verify_library_claims` and `list_library_surface` read it and fail with `Sidecar not initialized` without it. The project itself is built lazily by the first of those requests, not by `init`.

`capture_v2`, `check_v2`, `build_workspace`, `check_compatibility`, `health` and `shutdown` are stateless — they build whatever they need from the request and do not touch the init'd project.

### Actions

| Action | Needs `init` | Purpose |
|---|---|---|
| `init` | — | Point the sidecar at a repo/service root |
| `capture_v2` | no | Emit a per-service declaration stub package |
| `check_v2` | no | Typecheck matched pairs across captured stubs |
| `infer` | yes | Resolve the type at a set of locators |
| `retype_check` | yes | Judge untyped consumer calls by retyping them with the producer's response |
| `verify_client_semantics` | yes | Check claims about an HTTP client library against its type declarations |
| `verify_library_claims` | yes | Check library claims of every role against the package's own declarations |
| `list_library_surface` | yes | List a package's declared surface the way the verifier reads it, with its full-surface hash |
| `resolve_definitions` | yes | As-written and structural form of captured aliases |
| `emit_surface` | yes | Emit a surface `.d.ts` with rewritten specifiers |
| `bundle` | yes | Legacy symbol bundling (superseded by `capture_v2`) |
| `build_workspace` | no | Assemble a synthetic monorepo from repo metadata |
| `check_compatibility` | no | Assignability checks inside such a workspace |
| `health` | no | Readiness and init cost |
| `shutdown` | no | Graceful exit |

#### `init` - Point the sidecar at a project

Resolves which tsconfig (or default patterns) the project will be built from and answers immediately. The ts-morph project itself is built by the first request that reads it — `bundle`, `emit_surface`, `infer` or `resolve_definitions` — and reused after that. So `init` costs the same on a repo with its dependencies installed as on a bare checkout, and the time a large program takes to build is charged to a request's deadline rather than to readiness (carrick#749).

Re-initialising re-scopes the sidecar to another root, and drops the previous project along with everything built over it.

```json
{
  "request_id": "1",
  "action": "init",
  "repo_root": "/absolute/path/to/repo",
  "tsconfig_path": "tsconfig.json"
}
```

`tsconfig_path` is optional and relative to `repo_root`. `tsconfig_snapshot` and `pinned_dependencies` are optional and carry another repo's compiler options / exact versions when the sidecar has to stand in for a tree it cannot see.

When the tsconfig `references` other projects, each file is typed under the project that owns it, found the way TypeScript's editor finds a file's project: the named config if it lists the file, else the first project its references reach (depth-first, in declared order) that does. `infer`, `bundle` and `retype_check` are answered by that project's program, built the first time a request needs it. `capture_v2` resolves each anchor in its owner's program and emits the surface once, under the project that owns the most anchors; an anchor from another project whose text names a module is demoted when the two projects' options differ. A file no project lists, and a request that names no file, uses the named tsconfig. A reference that cannot be read is skipped and reported (carrick#1604).

#### Deno projects

Unless a TypeScript config is explicitly selected, `init` and `capture_v2` discover
`deno.json` or `deno.jsonc`, including the containing Deno workspace. Pass the
service directory as `repo_root` and omit `tsconfig_path`. An explicit Deno
config must name the nearest Deno manifest for that service. An explicitly
selected ordinary TypeScript config or init snapshot keeps
the existing TypeScript loader.

Deno **2.9.4** must be installed on `PATH`. Prepare each Deno root with
`deno install --frozen --node-modules-dir=none` before scanning. Mixed roots
also need their normal npm installation for services using an explicit
TypeScript config. Carrick reads [Deno's module graph](https://docs.deno.com/runtime/reference/cli/info/)
with `deno info --json --frozen --node-modules-dir=none`, and resolves npm
exports and transitive types from its `npmPackages.localPath` cache entries.
This works without project `node_modules`. Root import maps, member overrides,
workspace exports, npm aliases, JSR redirects and type overrides come from
Deno. Missing dependencies remain unresolved, with details in capture errors,
the sidecar log and generated `resolution-diagnostics.json`. Forcing
`node-modules-dir=none` prevents application lifecycle scripts even when the
Deno config contains `allowScripts`.

The installed CLI's [`deno types`](https://docs.deno.com/runtime/reference/cli/types/)
supplies runtime declarations. The supported
runtime scopes are `deno.window` and `deno.ns`; `deno.unstable` uses declarations
present in that CLI's output. Worker scopes and arbitrary runtime library
combinations are unsupported. The sidecar uses its bundled TypeScript compiler,
so this is not a replacement for `deno check` or a promise to support every
Deno runtime API. Node builtin ambient types that are absent from `deno types`
need explicit type declarations; generated application clients must exist before
scanning. Neither is replaced with synthetic declarations.

Preparation writes only generated files under `.carrick/deno`. No source
`package.json` or user-maintained TypeScript config is required. Capture
rewrites resolved imports into the declaration tree, pins referenced npm
packages and scopes reachable runtime declarations to the producer. The
compatibility checker can then read the stubs without installing Deno.
If one captured tree references multiple versions of the same npm package,
capture fails explicitly because a single dependency pin cannot preserve both.

The Deno integration tests in `test/deno-project.test.ts` require Deno on
`PATH`; they report a skip when it is unavailable. CI must install the exact
version above so these tests execute. Run them after building:

```bash
node --test dist/test/deno-project.test.js
```

Response:
```json
{ "request_id": "1", "status": "ready", "init_time_ms": 523 }
```

#### `capture_v2` - Emit a service's declaration stubs

The v2 "tsc as serializer" capture. Stateless: it builds its own program from the service's tsconfig, aliases each anchor into a surface entry, and writes a stub package (declaration tree + `carrick-manifest.json` + exact-version pins) into `out_dir`. The full contract is `src/capture/api.ts`.

An anchor is one of four kinds, discriminated on `kind`:

```json
{
  "request_id": "2",
  "action": "capture_v2",
  "repo_root": "/absolute/path/to/repo",
  "service_name": "orders-api",
  "out_dir": "/absolute/path/to/.carrick/stubs/orders-api",
  "tsconfig_path": "tsconfig.json",
  "anchors": [
    {
      "kind": "symbol",
      "alias": "Endpoint_a1b2_Response",
      "symbol_name": "Order",
      "source_file": "src/types/order.ts",
      "anchor_origin": "llm-symbol",
      "array_depth": 1
    },
    {
      "kind": "handler_return",
      "alias": "Endpoint_c3d4_Response",
      "symbol_name": "getOrder",
      "source_file": "src/routes/orders.ts",
      "anchor_origin": "llm-symbol"
    },
    {
      "kind": "infer",
      "alias": "Endpoint_e5f6_Request",
      "source_file": "src/routes/orders.ts",
      "anchor_origin": "deterministic-infer",
      "line_number": 42,
      "expression_text": "req.body",
      "unwrap": "awaited"
    },
    {
      "kind": "literal",
      "alias": "Endpoint_0011_Response",
      "type_text": "{ ok: boolean }",
      "anchor_origin": "anchor-backfill",
      "source_file": "src/routes/orders.ts"
    }
  ]
}
```

`anchor_origin` is one of `llm-symbol`, `deterministic-infer`, `anchor-backfill`,
`manifest-placeholder`. The last one is an alias the driver's type manifest
declares but no type request reached: it is sent as a literal `unknown` so the
surface carries every alias the check will import, and the check's IsUnknown
gate reports the type as unknown rather than the export as missing.
A literal anchor's optional `source_file` names the file its text was printed
from; it joins the analysis program so names the text prints bare are found
declared, and is omitted for inline text that has no file.

Response (`result` is a `CaptureStubResult`, abbreviated here):
```json
{
  "request_id": "2",
  "status": "success",
  "result": {
    "success": true,
    "stub_dir": "/absolute/path/to/.carrick/stubs/orders-api",
    "package_name": "@carrick/orders-api",
    "emitted_files": ["types/surface.d.ts", "types/src/types/order.d.ts"],
    "pinned_dependencies": { "zod": "3.23.8" },
    "unpinned_externals": [],
    "aliases": [
      {
        "alias": "Endpoint_a1b2_Response",
        "anchor_kind": "symbol",
        "symbol_name": "Order",
        "source_file": "src/types/order.ts",
        "anchor_origin": "llm-symbol",
        "serialization": "declaration_emit",
        "self_check": "ok"
      }
    ],
    "bare_checkout": false,
    "ts_version": "5.8.3",
    "errors": []
  }
}
```

#### `check_v2` - Judge matched pairs

Assembles the given stub packages into a scratch synthetic monorepo, installs the pins, and typechecks one generated probe per pair. Stateless. This is the only action that answers with more than one frame: while it installs and checks it emits `status: "progress"` keepalives, then exactly one terminal `success` or `error` frame. A client must skip progress frames rather than treat the first frame as the answer.

```json
{
  "request_id": "3",
  "action": "check_v2",
  "stubs": [
    { "service_name": "orders-api", "stub_dir": "/abs/.carrick/stubs/orders-api" },
    { "service_name": "web", "stub_dir": "/abs/.carrick/stubs/web" }
  ],
  "pairs": [
    {
      "pair_key": "http|GET|/orders/:id",
      "protocol": "http",
      "type_kind": "response",
      "producer": { "service_name": "orders-api", "alias": "Endpoint_a1b2_Response" },
      "consumer": { "service_name": "web", "alias": "Endpoint_9f8e_Response" }
    }
  ],
  "keep_workspace": false
}
```

`protocol` is one of `http`, `graphql`, `socket`, `pubsub`; `type_kind` is `request`, `response` or `both`. `workspace_root` is optional (defaults to the OS temp dir); `keep_workspace` keeps the assembled tree on disk.

Progress frames, zero or more:
```json
{ "request_id": "3", "status": "progress", "phase": "installing", "message": "check installing" }
```

Terminal frame (`result` is a `CheckResult`, abbreviated):
```json
{
  "request_id": "3",
  "status": "success",
  "result": {
    "success": true,
    "workspace_dir": "/tmp/carrick-check-xxxx",
    "isolation": "pnpm",
    "install_ok": true,
    "ts_version": "5.8.3",
    "verdicts": [
      {
        "pair_id": "8f1c0a2b",
        "pair_key": "http|GET|/orders/:id",
        "bucket": "compatible",
        "codes": []
      }
    ],
    "degraded_services": [],
    "errors": []
  }
}
```

A verdict's `diagnostic` (present on a mismatch) is the compiler's own text, followed by the fields that differ: which side does not send a field the other requires, which field is sent under a name the other does not declare, which are optional on one side and always present on the other, and where two member types disagree. The list is capped and says how many it did not name. It is walked over the same two types the judge compared, with the compiler's own assignability relation, so it never names a field the verdict does not rest on and never changes a verdict.

An `http` pair is judged on the form JSON puts on the wire. A value with a `toJSON()` method travels as what it serialises to, so a producer returning `Date` where the consumer reads `string` is not a drift, and a consumer that sends a `Date` in a request body satisfies a producer declaring `string`. The transform applies to the SENDING side in each direction, which is where serialisation happens: a consumer declaring `Date` for a response is still a mismatch, because no `Date` ever arrives. `bigint` is left alone — `JSON.stringify` throws on one, so it is a real problem, not a wire difference.

A verdict that is not a fact (`resolved: false`) says which side is to blame in `unresolved_side` (`producer` or `consumer`) when one is: a gate, a missing export, poison or a deep `any`/`unknown` on that side. It is absent when neither side is to blame.

#### `retype_check` - Judge an untyped consumer call

A consumer call with no type argument (`await api.post('/orders')`) returns `any`, so check_v2 has nothing to compare. `retype_check` rewrites the call in memory to state the producer's response type, type-checks the consumer file before and after, and reports every diagnostic the rewrite added. Each one is a place the consumer uses something the producer does not return. It runs in the init'd project, because the consumer's file only type-checks there, and it restores the file before it answers.

The call is located the way `infer` locates a `call_result`: by span, else by `expression_text` near `expression_line`. A call that already states a type argument has it replaced. A call to a generic gets one inserted. A call with no type parameter is not retyped. `wire: true` compares the form JSON puts on the wire, as check_v2 does for `http`.

```json
{
  "request_id": "5",
  "action": "retype_check",
  "items": [
    {
      "item_id": "web/Endpoint_9f8e_Response",
      "file_path": "/abs/web/src/orders.ts",
      "line_number": 12,
      "expression_text": "api.post('/orders')",
      "expression_line": 12,
      "producer_type": "{ id: string; total: number; }",
      "wire": true
    }
  ]
}
```

Response:
```json
{
  "request_id": "5",
  "status": "success",
  "outcomes": [
    {
      "item_id": "web/Endpoint_9f8e_Response",
      "outcome": "mismatch",
      "diagnostics": [
        { "line": 13, "code": 2339, "message": "Property 'totalMinutes' does not exist on type '{ id: string; total: number; }'." }
      ]
    }
  ]
}
```

`budget_ms` (optional, default 600000) caps the time one request spends; the items it does not reach abstain. `outcome` is `mismatch`, `agrees`, `wider` or `abstain`. `wider` answers an item that also carries `producer_unwidened_type`, the producer's response as its handler returns it with no literal widened (an `infer` response's `unwidened_type_string`): the published type added diagnostics and this one added none, so the producer's type is wider than what it sends (carrick#1516). Its `diagnostics` are the published type's. An abstention carries a `reason`: the call was not found, nothing reads its result, its result escapes to readers outside the file's own type-check (returned from a function with no declared return type, or bound to an exported name), the type parameter it would fill does not carry the response, the call takes no type parameter, the producer's type names something the consumer's program cannot resolve, or the producer's type, as the consumer's program reads it, is `unknown` as a whole or at a member, or is `any` there and the rewrite added no diagnostic (the reason names the member, e.g. `'unknown' at 'session'`). An `any` member can hide a diagnostic but never add one, so the diagnostics found beside it still make a `mismatch`. Diagnostics the file had before the rewrite never count.

#### `verify_client_semantics` - Check a client library's claimed call shapes

The scanner reads a call through an HTTP client library (its base-URL factory, its verbs, its request call) only after this action has checked each claim about that library against the package's own type declarations (carrick#1564). A claim is checked on a receiver: `export`, the export itself, or `instance:<factory>`, what that factory member of the export returns. Module resolution starts at `from_dir`, the absolute service root, under the compiler options `init` resolved: a probe file there imports the export, inside the service's program, and is removed before the action answers.

```json
{
  "request_id": "6",
  "action": "verify_client_semantics",
  "from_dir": "/abs/web",
  "checks": [
    {
      "claim_id": "@fixture/http@1:default:factory:create",
      "package": "@fixture/http",
      "export": "default",
      "receiver": "export",
      "claim": { "kind": "factory", "member": "create", "base_url_key": "baseURL" }
    },
    {
      "claim_id": "@fixture/http@1:default:verb:post",
      "package": "@fixture/http",
      "export": "default",
      "receiver": "instance:create",
      "claim": { "kind": "verb", "member": "post", "method": "POST" }
    },
    {
      "claim_id": "@fixture/http@1:default:request:():config",
      "package": "@fixture/http",
      "export": "default",
      "receiver": "export",
      "claim": { "kind": "request", "member": null, "args": "config", "url_key": "url", "method_key": "method" }
    },
    {
      "claim_id": "@fixture/http@1:default:verb:fetch",
      "package": "@fixture/http",
      "export": "default",
      "receiver": "export",
      "claim": { "kind": "verb", "member": "fetch", "method": "GET" }
    }
  ]
}
```

Response:
```json
{
  "request_id": "6",
  "status": "success",
  "semantics": [
    { "claim_id": "@fixture/http@1:default:factory:create", "receiver": "export", "verdict": "verified" },
    { "claim_id": "@fixture/http@1:default:verb:post", "receiver": "instance:create", "verdict": "verified" },
    { "claim_id": "@fixture/http@1:default:request:():config", "receiver": "export", "verdict": "verified" },
    { "claim_id": "@fixture/http@1:default:verb:fetch", "receiver": "export", "verdict": "failed", "reason": "method_not_member_verb" }
  ],
  "semantics_modules": [
    { "package": "@fixture/http", "resolved_file": "/abs/web/node_modules/@fixture/http/index.d.ts", "installed_version": "1.4.2" }
  ]
}
```

`semantics` holds exactly one result per check, in request order. The claim kinds are `factory`, `verb`, `verb_body` (`args` `path_body` or `path_options`, optional `body_key`), `request` (`member` null means the receiver itself is called; `args` `config` with `url_key`, or `path_options`; `method_key`) and `request_body` (the same `member`, `args`, `url_key` and `method_key` as its `request` claim, plus `body_key`). `verdict` is `verified` (the declarations satisfy the claim), `failed` (they resolved and contradict it) or `unchecked` (they could not be read); `reason` is absent exactly when `verified`. The scanner drops `failed` and `unchecked` alike.

A verified claim becomes a fact the scanner can report on, so a claim verifies only where the declarations say so positively. The definitions the predicates use:

- **Accepts string**: `string` is assignable to the type, and some part of it other than `null` and `undefined` is string-like (`string`, a string literal or template, or `string & {}`). `any`, `unknown`, `{}` and `Object` accept a string without saying anything about one.
- **Declared property**: a property the checker lists on the type's apparent type, with `null` and `undefined` removed. It does not count when:
  - it comes from an index signature;
  - it is a member of `Object`, `Function`, `Array` or a primitive's wrapper (`constructor`, `toString`, a string's `length`);
  - it is typed `never`, or only `undefined`;
  - none of its declarations is in an installed package or the default library, so a member only the service's own module augmentation adds does not count. A member a mapped type makes has no declaration of its own. It counts when it reaches the type along a path of types the library alone declares: an intersection's parts that list it (`type Client = {...} & Record<Alias, Fn> & Fn`), or an interface's base types that list it, at any depth. It does not count when the service's own `declare module '<this package>'` (or subpath) or `declare global` block declares a member of that name, compared without case: the service can add a key to the interface a library mapped type iterates (`Record<keyof MethodMap, Fn>`).

A member's call signatures count the same way: an overload the service adds to a library member is not the library's.
- **Open** (a `path_body` body): `unknown`, or a type parameter with no constraint (or a constraint of `unknown` or `any`). `any` is not open: a declared `any`, an unresolved type and a defaulted type argument all read as `any`.
- **Says nothing**: `any`, `unknown`, an unconstrained type parameter, an object type with nothing declared on it, or an element of a rest parameter typed by a type parameter. Where the claim needs a type that says nothing, the verdict is `unchecked` (`member_untyped`), not `failed`.
- **Where a key or a string is looked for**: a parameter is read through a rest parameter's element and through a type parameter's constraint, so `extend<T extends Array<Instance | Options>>(...items: T)` is read as `Instance | Options`. A key the whole type does not declare may come from one constituent of a union, when that constituent is an object type (not a function type) and the key there passes the check the claim needs (accepts string, or a method literal for a method key). So `extend(defaults: Options | ((parent: Options) => Options))` reads its base-URL key through `Options`. A request's keys (url, method, and a `request_body` claim's body key) must all come from ONE config object: for a union, from one member that is an object type and not a function type. Keys split across members (`{ url } | { method }`) describe no call anyone can make. A `verb_body` claim's body key keeps the whole-type rule, and an open body is read as declared.

The package's declarations must be the installed package's. A file is installed when its path, relative to the service root (symlinks resolved), has a package directory after its last `node_modules` segment (two segments for a scope) holding a `package.json`. That covers a pnpm store and an install hoisted above the service root. A service source file in a directory that happens to be named `node_modules`, or a repository checked out under a `node_modules` ancestor, is not installed. The default library counts as installed.

The resolver and the checker must land on the same installed file. With two installs of the same name and version, TypeScript makes one a redirect to the other. The resolver's copy may then be a redirect whose target is the checker's file, provided that target is itself installed. The installed package must also be the one named: by the resolver's `packageId`, by its separate `@types` package (`@types/scope__name` for a scoped one), or, for an `npm:` alias, by the directory the installer named after the alias while its `package.json` is the one `packageId` names. A `paths` alias that points the name at another installed package, or at a copy outside `node_modules`, is not it. Under pnpm the resolver returns the alias target's realpath, whose directory names the target, so a pnpm alias reads `module_local`. The reasons:

| Verdict | Reason | Meaning |
|---|---|---|
| `unchecked` | `module_unresolved` | The package does not resolve from `from_dir` (not installed, no `node_modules`) |
| `unchecked` | `module_js_only` | It resolves to JavaScript with no declarations |
| `unchecked` | `module_local` | Its types are not the named installed package's: a `paths` alias (to the service's code or to another installed package), a `declare module` block (a shorthand one included) that stands in for the package, or a pnpm `npm:` alias |
| `unchecked` | `export_missing` | The module has no value export of that name |
| `unchecked` | `export_untyped` | The export is `any` or `unknown` |
| `unchecked` | `factory_unresolved` | `instance:<f>`: `f` is not a callable member, no overload builds from an options object, what it builds says nothing (including a conditional return with an `any` branch), or a `factory` claim for `f` in the same request holds on a different overload. Also a `factory` claim whose declared option is there but whose result says nothing |
| `unchecked` | `member_untyped` | The member, parameter or key the claim needs is typed so that it says nothing |
| `unchecked` | `receiver_invalid` | The receiver is neither `export` nor `instance:<factory member>` |
| `unchecked` | `budget` | The request ran out of `budget_ms` (optional, default 600000) before this check |
| `failed` | `member_missing`, `member_not_callable` | The member is not declared on the receiver, or has no call signature |
| `failed` | `method_not_member_verb` | A `verb` claim's method is not its member upper-cased (ASCII letters only), or not `GET POST PUT PATCH DELETE HEAD OPTIONS` |
| `failed` | `path_not_string` | No signature of the verb takes a string first |
| `failed` | `param_missing` | No signature has the parameter the claim needs there (a request's config must be an object type) |
| `failed` | `body_not_open` | A `path_body` verb's body parameter is not open |
| `failed` | `options_not_object` | A `path_options` verb's second parameter is not an object type with a declared property |
| `failed` | `key_missing`, `key_not_string` | The claimed key is not a declared property, or does not accept a string (a method key may accept an HTTP method literal instead) |

When no overload satisfies a claim, the reason is the one from the overload that got furthest, and at the same depth `unchecked` wins. A `request_body` claim reads the signatures its `request` claim selects. `semantics_modules` says how each package resolved, for logs.

#### `verify_library_claims` - Check library claims of every role (carrick#1616)

The wire is pinned on carrick#1564 (comment 5937606126, section 3, as amendment 2, comment 5939543981, changes it); where this section and the contract disagree, the contract wins. A claim says what a package's export does (its role, from a closed list) and where each part of a call through it sits, and the role alone picks which checks it needs. Same probe file, module resolution rules and definitions ("declared property", "accepts string", "says nothing") as `verify_client_semantics`, which converts each of its checks into this shape and answers through the same code (`http_client` keeps the #1564 checks and reasons exactly).

```json
{
  "request_id": "7",
  "action": "verify_library_claims",
  "from_dir": "/abs/worker",
  "budget_ms": null,
  "checks": [
    {
      "claim_id": "@fixture/queue@2:default:make:task",
      "package": "@fixture/queue",
      "export": "default",
      "role": "broker",
      "receiver": "export",
      "claim": { "kind": "make", "form": "call", "member": "task", "name": { "arg": 0, "key": "id" }, "handler": { "arg": 0, "key": "run" },
                 "key_labels": { "id": "name", "description": "not_name" },
                 "name_scope": { "scope": "service", "namespace": "task" }, "picker": "<model>/<question set>" }
    },
    {
      "claim_id": "@fixture/queue@2:default:op:send:trigger:instance",
      "package": "@fixture/queue",
      "export": "default",
      "role": "broker",
      "receiver": "instance:task",
      "claim": { "kind": "op", "op": "send", "member": "trigger", "of": "instance:task",
                 "name": { "bound": "maker" }, "payload": { "arg": 0 } }
    },
    {
      "claim_id": "@fixture/queue/v2@2:default:op:send:trigger:export",
      "package": "@fixture/queue/v2",
      "export": "default",
      "role": "broker",
      "receiver": "export",
      "claim": { "kind": "op", "op": "send", "member": "trigger", "path": ["tasks"], "on": "export",
                 "name": { "arg": 0 }, "payload": { "arg": 1 } }
    }
  ]
}
```

A slot is `{ "arg": n }` (argument `n`, 0-based) or `{ "arg": n, "key": "k" }` (property `k` of the object passed there). An op's `name` may instead be `{ "bound": "maker" }` (the maker's `name` slot bound it to the instance) or `{ "bound": "scope" }` (the scope member bound it). `picker` and `name_scope` travel with the claim and are never read.

| `kind` | Fields | Says |
|---|---|---|
| `make` | `form` (`call` or `new`), `member` (null: the export itself), `base?`, `prefix?`, `name?`, `handler?` (each a slot), `key_labels?` | How an instance is made. A definition (`task({ id, run })`) is a maker with a keyed `name` and `handler`; a queue (`new Queue("emails")`) is one with a positional `name`. |
| `scope` | `member`, `name`, `path?`, `on?`, `of?`, `key_labels?` | A member that returns a receiver bound to a name (a channel, room or queue) |
| `op` | `op` (`request`, `send`, `receive`, `execute`), `member` (null: the object itself), `path?`, `on?`, `of?`, `name?`, `payload?`, `handler?`, `ack?`, `key_labels?`; HTTP only: `method?`, `method_key?`, `options?` | A member that acts on the wire |
| `reserved` | `member`, `name`, `path?`, `on?`, `of?` | A name the library emits itself, spelled in one of that member's parameters |

`path` is the member path from the receiver to the object `member` sits on (`client.tasks.trigger`). An op, scope or reserved name says which receivers it is for with `on` or `of`, never both: `on` is `export`, `instance` (every instance of every maker) or `both`; `of` is exactly one receiver, by its receiver id (`instance:new`, `instance:connect>scope:channel`). A member on the export and on one maker's instances is two elements.

The receiver is `export`, `instance:<member>` (what `export.member(...)` returns), `instance:()` (what calling the export returns), `instance:new` (what `new export(...)` builds) or `instance:new:<member>` (what `new export.member(...)` builds), optionally followed by `>scope:<path.member>` (what that scope member returns on it, its `path` joined by `.`).

Rules for the message roles (`broker`, `in_process_bus`, `socket`):

| Rule | Reason when it does not hold |
|---|---|
| An element carries `on` or `of`, not both, and `of` is a receiver id a maker or scope builds (not `export`) | `unchecked claim_invalid` |
| A maker is checked on the receiver `export`; an element with `of` only on that receiver; one `on` the export only there, `on` instances only on an instance; `on` never on a receiver a scope returns | `unchecked receiver_invalid` |
| A member (and each `path` hop) is declared by the receiver's home packages: the named package and its `@types` package, the packages its export is re-exported from, and the package that declares the receiver's type (a hop adds the package that declares its type). Never the runtime's type packages, never only another package's base. A base the class binds with a concrete type its home packages declare (`extends Emitter<L, E, OwnReservedEvents>`) counts as declared. | `failed member_inherited` |
| A claim that names a runtime module itself (`node:events`) reads the runtime's types package's ambient `declare module` block, reports that package's installed version, and has that package as its home (contract amendment 1, A1). A bare name (`events`) is a registry package's, a `node:` block only the service writes is not the runtime's, and HTTP reads `node:` as #1564 does. | `unchecked module_local` |
| A hop typed `any`, `unknown` or `{}` | `unchecked member_untyped` |
| The name slot accepts a string and is not a key of a concrete index-signature map. A key of a map the library takes as a type parameter (an event map defaulting to an index signature) reads as a string slot (amendment 2, B6). | `failed name_not_string` / `failed name_index_key` |
| A positional `base` or `prefix` accepts a string | `failed slot_not_string` |
| Strict D2: every other string-accepting argument or key at the call is assigned a part by the claim or labelled `not_name` in `key_labels`; at most one key is labelled `name`, and only the claim's own name key. A positional string cannot be labelled, so it always competes. A `not_name` label for a key the overload does not declare is ignored. | `failed name_ambiguous` |
| A `send` carries a payload slot; the name alone, or no name, describes no call | `unchecked claim_invalid` |
| A callback is not a payload; name and payload never share an argument | `failed payload_is_function` / `failed slots_overlap` |
| Handler and ack slots are functions with a declared signature (`Function`, `any`, `unknown`, `(...args: any[])` say nothing) | `unchecked handler_untyped` / `failed handler_not_function` |
| Keys at one argument come from one union member | `failed key_missing` |
| A maker is read on every overload that holds (across every maker claim of the request for it); each instance op must hold on every instance type those overloads return. A generic maker or scope is read at its declared type-parameter defaults (amendment 2, B6): what TypeScript gives a call with no argument and no type argument, so a default, else a constraint `unknown` does not satisfy, else `unknown`. A maker returning `any`, or a generic with no default and no constraint, builds nothing | `unchecked maker_unresolved` |
| Instance and scope ops travel in the same request as a maker or scope claim that verifies | `unchecked maker_unverified` / `unchecked scope_unverified` |
| A name `{ "bound": ... }` needs a maker with a `name` slot, or a scope | `failed name_unbound` |
| An export given two roles in one request | `unchecked role_conflict` |
| A `workspace:`, `file:`, `link:` or `portal:` dependency (until its own source is verified, #1666) | `unchecked module_workspace` |
| `graphql_client`, `server_framework` and `none`, and an `execute` op on a message role | `unchecked role_unsupported` |

Not refusable by shape (carrick#1653): an in-process emitter classified as a socket, a raw `send(data, options)` with a wrong name-at-0 claim, and a generic event map defaulting to an index signature.

Response: `verdicts` (exactly one per check, in request order, the same objects as `verify_client_semantics` answers in `semantics`), `modules` (how each package resolved) and `duration_ms`.

```json
{
  "request_id": "7",
  "status": "success",
  "verdicts": [
    { "claim_id": "@fixture/queue@2:default:make:task", "receiver": "export", "verdict": "verified" },
    { "claim_id": "@fixture/queue@2:default:op:send:trigger:instance", "receiver": "instance:task", "verdict": "verified" },
    { "claim_id": "@fixture/queue/v2@2:default:op:send:trigger:export", "receiver": "export", "verdict": "failed", "reason": "member_missing" }
  ],
  "modules": [
    { "package": "@fixture/queue", "resolved_file": "/abs/worker/node_modules/@fixture/queue/index.d.ts", "installed_version": "2.4.1" },
    { "package": "@fixture/queue/v2", "resolved_file": "/abs/worker/node_modules/@fixture/queue/v2/index.d.ts", "installed_version": "2.4.1" }
  ],
  "duration_ms": 412
}
```

#### `list_library_surface` - List a package's declared surface (carrick#1660)

Lists each specifier the way `verify_library_claims` reads it, from the same probe file, module resolution and definitions, so a claim chosen from the listing names a slot the verifier indexes the same way. The library store runs it on every package it answers for, as the released artifact (see "The surface lister artifact").

```json
{
  "request_id": "8",
  "action": "list_library_surface",
  "from_dir": "/abs/worker",
  "packages": ["@fixture/queue", "@fixture/queue/v2"],
  "max_entries": 1000,
  "exports": { "@fixture/queue": ["default"] }
}
```

`packages` are module specifiers: a package, its subpaths, or a runtime module (`node:events`, listed from the runtime's types package, as the message roles read it). `exports` (optional) lists only the named exports of a specifier. `max_entries` (default 1000) caps each specifier; exports, receivers, members, signatures, parameters and keys each count one. Every export and receiver is listed before any member name, and every member name before any signature, so the cap cuts signatures first.

Per specifier, the listing holds each value export (sorted by name; a module that exports a value whole with `export =` also lists `default`, the value a default import gets, when the program compiles that import), and per export the receivers a claim can be read on, in the verifier's grammar: `export`; `instance:()` and `instance:new` when calling or constructing the export, through the signatures a maker claim reads there, builds one object type (overloads whose return says nothing aside); `instance:<member>` and `instance:new:<member>` when calling or constructing one of its members does. A made receiver is read as the verifier reads it, built with no argument and no type argument, so a generic maker's instance is listed at its declared type-parameter defaults (carrick#1696). Every field is read with the verifier's own predicate. Each receiver lists the call and construct signatures a claim with a null member reads, and its callable members, with `own` when an op claim reads the member as the receiver's own (`ownCallee`: its home packages declare it, or it sits on another package's base the receiver binds with its own type, and they write a signature of it). An own member lists the signatures the verifier reads; an inherited one, every library signature. Each signature lists its parameters: `optional`, `rest`, printed `type` (unions in canonical order), `accepts_string` and `function` (as a positional name and handler are checked there: through a rest's element, a type parameter's constraint and a conditional's branches), the `keys` a claim can name at an options object (of each union member that is an object type, each with `optional`, `accepts_string` and `function`), and the string `literals` the slot spells. A member or key keyed by a symbol (`[Symbol.iterator]`) is not listed: no claim can name it. Type text names `from_dir` as `<root>`.

A specifier that lists nothing carries the verifier's module `reason` (`module_unresolved`, `module_local`, `module_js_only`). `surface_sha256` is the full-surface hash the store keys on: the sha256 of the JSON array of `[package, exports]` for every specifier that listed at least one export, sorted by `package`. The same packages hash the same in any directory.

```json
{
  "request_id": "8",
  "status": "success",
  "surfaces": [
    {
      "package": "@fixture/queue",
      "resolved_file": "/abs/worker/node_modules/@fixture/queue/index.d.ts",
      "installed_version": "2.4.1",
      "truncated": 0,
      "exports": [
        {
          "export": "default",
          "receivers": [
            { "receiver": "export", "members": [
              { "name": "task", "own": true, "signatures": [
                { "params": [
                  { "name": "options", "optional": false, "rest": false, "type": "TaskOptions<unknown>", "accepts_string": false, "function": false,
                    "keys": [
                      { "name": "id", "optional": false, "accepts_string": true, "function": false },
                      { "name": "run", "optional": false, "accepts_string": false, "function": true }
                    ] }
                ], "returns": "Task<unknown>" }
              ] }
            ] },
            { "receiver": "instance:task", "members": [
              { "name": "trigger", "own": true, "signatures": [
                { "params": [
                  { "name": "payload", "optional": false, "rest": false, "type": "unknown", "accepts_string": false, "function": false }
                ], "returns": "Promise<RunHandle>" }
              ] }
            ] }
          ]
        }
      ]
    },
    { "package": "@fixture/queue/v2", "truncated": 0, "exports": [], "reason": "module_unresolved" }
  ],
  "surface_sha256": "<sha256>"
}
```

#### `infer` - Resolve the type at a locator

Each item locates one expression. The fields are `file_path`, `line_number` and `infer_kind`; a locator is completed by a span (`span_start` + `span_end`), by `expression_text` (+ optional `expression_line`), or by the line alone for the kinds that anchor on a function (`function_return`, `signature_return`, `function_param`, `response_body`, `request_body`). Anything else is rejected per item, and that item alone pads to `unknown` — a bad item never sinks the batch.

`infer_kind` is one of `function_return`, `expression`, `call_result`, `variable`, `response_body`, `request_body`, `signature_return`, `function_param`, `receiver_type`.

`extraction_config` carries the caller's unwrap rules (wrapper symbols, origin module globs, payload paths). **Live behaviour depends on it**: without it the inferrer cannot unwrap a framework envelope, so a probe written without one does not reproduce what a real scan sees.

A `response_body` or `function_return` answer can also carry `unwidened_type_string`: the same inference read again with every literal on the handler's path kept at its literal type (`scope: "all" | "specific"` where `type_string` says `scope: string`). It is absent when the two are the same, and when keeping the literals added a diagnostic on the path. The reading of one batch stops after 120 seconds and keeps the answers it has.

```json
{
  "request_id": "4",
  "action": "infer",
  "requests": [
    {
      "file_path": "src/routes/users.ts",
      "line_number": 25,
      "infer_kind": "response_body",
      "alias": "Endpoint_7a6b_Response",
      "expression_text": "res.json(users)",
      "expression_line": 25
    },
    {
      "file_path": "src/routes/users.ts",
      "line_number": 40,
      "infer_kind": "function_param",
      "param_name": "body"
    }
  ],
  "extraction_config": {
    "rules": [
      {
        "wrapperSymbols": ["ApiResponse"],
        "originModuleGlobs": ["**/lib/http.ts"],
        "payloadGenericIndex": 0,
        "unwrapRecursively": true
      }
    ]
  }
}
```

Response:
```json
{
  "request_id": "4",
  "status": "success",
  "inferred_types": [
    {
      "alias": "Endpoint_7a6b_Response",
      "type_string": "{ id: string; name: string; }[]",
      "is_explicit": false,
      "infer_kind": "response_body",
      "source_location": { "file_path": "src/routes/users.ts", "start_line": 25, "end_line": 25 }
    }
  ],
  "errors": []
}
```

#### `resolve_definitions` - Read aliases out of a capture stub

Resolves surface aliases from a stub package's declaration tree (`<stub_dir>/types/surface.d.ts`), in a dedicated throwaway project so the warm project never sees stub files. Returns two forms per alias: `definition` as written (named refs preserved) and `expanded` fully structural, with named members inlined. Union members print in a canonical order, so the same tree always yields the same string (carrick#735).

An alias that does not resolve is skipped, not failed.

```json
{
  "request_id": "5",
  "action": "resolve_definitions",
  "stub_dir": "/absolute/path/to/.carrick/stubs/orders-api",
  "aliases": ["Endpoint_a1b2_Response", "Endpoint_c3d4_Request"]
}
```

Response:
```json
{
  "request_id": "5",
  "status": "success",
  "definitions": [
    {
      "type_alias": "Endpoint_a1b2_Response",
      "definition": "interface Order { id: string; total: Money }",
      "expanded": "{ id: string; total: { amountCents: number; currency: string; }; }"
    }
  ]
}
```

#### `emit_surface` - Emit a surface `.d.ts`

Writes one file declaring each payload as an alias, with import specifiers rewritten so the file stands alone. Needs an init'd project.

```json
{
  "request_id": "6",
  "action": "emit_surface",
  "repo_name": "orders-api",
  "output_path": "/absolute/path/to/.carrick/surface/orders-api.d.ts",
  "payloads": [
    {
      "alias": "Endpoint_a1b2_Response",
      "type_string": "Order",
      "source_file": "src/types/order.ts"
    }
  ]
}
```

Response:
```json
{
  "request_id": "6",
  "status": "success",
  "output_path": "/absolute/path/to/.carrick/surface/orders-api.d.ts",
  "surface_content": "export type Endpoint_a1b2_Response = Order;",
  "manifest": [
    { "alias": "Endpoint_a1b2_Response", "type_string": "Order", "rewritten_imports": [] }
  ]
}
```

#### `bundle` - Legacy symbol bundling

Superseded by `capture_v2`, which emits through the compiler instead of reprinting declarations. Kept for the paths that still call it. Needs an init'd project.

`source_file` may declare the symbol or re-export it (`export *` at any depth, `export { X } from`, `export { X as Y } from`); the bundle reads the declaration it resolves to. A name that two `export *` sources both provide is a symbol failure (carrick#1605).

```json
{
  "request_id": "7",
  "action": "bundle",
  "symbols": [
    {
      "symbol_name": "User",
      "source_file": "src/types/user.ts",
      "alias": "Endpoint_1122_Response",
      "array_depth": 1
    }
  ]
}
```

Response:
```json
{
  "request_id": "7",
  "status": "success",
  "dts_content": "export type Endpoint_1122_Response = { id: string; name: string; }[];",
  "manifest": [{ "alias": "Endpoint_1122_Response", "type_string": "{ id: string; name: string; }[]" }],
  "symbol_failures": []
}
```

#### `build_workspace` - Assemble a synthetic monorepo

Writes a workspace holding one stub package per repo, from metadata alone (no checkout required). Stateless.

```json
{
  "request_id": "8",
  "action": "build_workspace",
  "workspace_root": "/absolute/path/to/.carrick/workspace",
  "repos": [
    {
      "repoName": "orders-api",
      "dependencies": { "zod": "3.23.8" },
      "tsconfig": { "compilerOptions": { "module": "ESNext", "strict": true } },
      "surfaceContent": "export type Endpoint_a1b2_Response = { id: string };"
    }
  ]
}
```

Response:
```json
{
  "request_id": "8",
  "status": "success",
  "workspace_path": "/absolute/path/to/.carrick/workspace",
  "stub_packages": ["/absolute/path/to/.carrick/workspace/packages/orders-api"],
  "checker_path": "/absolute/path/to/.carrick/workspace/packages/checker"
}
```

#### `check_compatibility` - Assignability inside a built workspace

```json
{
  "request_id": "9",
  "action": "check_compatibility",
  "workspace_root": "/absolute/path/to/.carrick/workspace",
  "checks": [
    {
      "source_repo": "orders-api",
      "source_alias": "Endpoint_a1b2_Response",
      "target_repo": "web",
      "target_alias": "Endpoint_9f8e_Response",
      "direction": "source_extends_target"
    }
  ]
}
```

`direction` is one of `source_extends_target`, `target_extends_source`, `bidirectional`.

Response:
```json
{
  "request_id": "9",
  "status": "success",
  "results": [
    {
      "source_repo": "orders-api",
      "source_alias": "Endpoint_a1b2_Response",
      "target_repo": "web",
      "target_alias": "Endpoint_9f8e_Response",
      "compatible": true
    }
  ],
  "diagnostics": []
}
```

#### `health` - Readiness

```json
{ "request_id": "10", "action": "health" }
```

Response — `ready` once `init` has resolved a project, `not_ready` before that:
```json
{ "request_id": "10", "status": "ready", "init_time_ms": 523 }
```

#### `shutdown` - Graceful exit

```json
{ "request_id": "11", "action": "shutdown" }
```

Response, written before the process exits:
```json
{ "request_id": "11", "status": "success" }
```

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                       TypeSidecar (Node.js)                      │
│                                                                  │
│  stdin ──► JSON parse ──► validate (zod) ──► route ──► stdout    │
│                                                                  │
│  project-backed (built lazily after init):                       │
│    TypeBundler · SurfaceEmitter · TypeInferrer                   │
│    DefinitionResolver                                            │
│                                                                  │
│  stateless (own program / own workspace per request):            │
│    capture/ (capture_v2, check_v2) · MonorepoBuilder             │
└─────────────────────────────────────────────────────────────────┘
```

## Files

| File | Description |
|------|-------------|
| `src/index.ts` | Entry point, message loop, dispatch, response/progress frames |
| `src/types.ts` | Request/response interfaces |
| `src/validators.ts` | Zod schemas; the authority on request shape |
| `src/project-loader.ts` | tsconfig resolution and ts-morph project construction |
| `src/bundler.ts` | Legacy symbol bundling and surface emission |
| `src/type-inferrer.ts` | Inference at a locator, with extraction-config unwrapping |
| `src/definition-resolver.ts` | Alias resolution out of a capture stub tree |
| `src/library-claims.ts` | `verify_library_claims`, `verify_client_semantics` and `list_library_surface`: library claims against a package's own declarations, and the surface they are read from |
| `lister/build.mjs` | Bundles the sidecar into the surface lister artifact and writes its manifest; esbuild is installed by `lister/package.json`, not the sidecar's own |
| `src/type-structural-expander.ts` | Shared structural rendering of a resolved type |
| `src/monorepo-builder.ts` | Synthetic workspace build and assignability checks |
| `src/capture/` | capture_v2 and check_v2; contract in `capture/api.ts` |
| `test/` | Compiled and run by `npm test` |

## Error Handling

`status` on every response:
- `"ready"` / `"not_ready"` — `init` and `health` only
- `"success"` — the request answered
- `"error"` — the request failed; read `errors`
- `"progress"` — a `check_v2` keepalive, never terminal

A request that fails validation answers with `status: "error"` and the zod message, e.g. `Invalid request: requests.0.infer_kind: Required`:
```json
{ "request_id": "4", "status": "error", "errors": ["Invalid request: ..."] }
```

Per-item failures inside `infer` and `bundle` do not fail the batch: they arrive as `errors` / `symbol_failures` beside the results that succeeded.

## Performance

- **Init**: resolution only, so it does not scale with the tree; the program is built by the first request that reads it
- **Warm requests**: fast for small batches; a first `infer` on a large program pays the build
- **Memory**: grows with the program; `CARRICK_SIDECAR_MAX_OLD_SPACE_MB` raises the heap cap

The Rust client's readiness budget is `CARRICK_SIDECAR_READY_TIMEOUT_SECS`.

## Debugging

stderr carries the log:

```bash
node dist/src/index.js 2>sidecar.log
```

```
[sidecar] Process started
[sidecar] Initializing with repo_root: /path/to/repo
[sidecar] Initialization complete in 523ms
[sidecar:error] Symbol not found: Foo
```

## See Also

- `src/services/type_sidecar.rs` — the Rust client
- `src/sidecar/src/capture/api.ts` — the capture/check contract (anchors, stub result, verdicts)
- `src/engine/type_compat_v2.rs` — how the driver builds pairs and reads verdicts
