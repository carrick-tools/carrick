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
npm run build   # tsc, output in dist/
npm test        # node --test over dist/test
```

`npm test` runs the COMPILED tests, so a stale `dist/` makes the suite lie. The Rust integration tests spawn `dist/src/index.js` for the same reason; the pre-commit hook rebuilds it.

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

`init` resolves a project; `bundle`, `emit_surface`, `infer` and `resolve_definitions` read it and fail with `Sidecar not initialized` without it. The project itself is built lazily by the first of those requests, not by `init`.

`capture_v2`, `check_v2`, `build_workspace`, `check_compatibility`, `health` and `shutdown` are stateless — they build whatever they need from the request and do not touch the init'd project.

### Actions

| Action | Needs `init` | Purpose |
|---|---|---|
| `init` | — | Point the sidecar at a repo/service root |
| `capture_v2` | no | Emit a per-service declaration stub package |
| `check_v2` | no | Typecheck matched pairs across captured stubs |
| `infer` | yes | Resolve the type at a set of locators |
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

#### Deno projects

When a service has no TypeScript config, `init` and `capture_v2` discover
`deno.json` or `deno.jsonc`, including the containing Deno workspace. Pass the
service directory as `repo_root` and omit `tsconfig_path`. An explicit Deno
config selects the same path. An ordinary TypeScript config or snapshot keeps
the existing TypeScript loader.

Deno must be installed on `PATH`. This implementation is tested with Deno
2.7.12. npm dependencies must already be available in the project's
`node_modules` tree. Carrick reads [Deno's module graph](https://docs.deno.com/runtime/reference/cli/info/)
with `deno info --json --frozen
--node-modules-dir=manual` to obtain per-file resolution, so root import maps,
member overrides, workspace exports, npm aliases, JSR redirects and type
overrides come from Deno. Remote dependencies use Deno's cache and normal
import permissions. Missing dependencies remain unresolved, with details in
the capture errors, sidecar log and generated `resolution-diagnostics.json`.
Carrick does not run install scripts or application code during preparation.

The installed CLI's [`deno types`](https://docs.deno.com/runtime/reference/cli/types/)
supplies runtime declarations. The supported
runtime scopes are `deno.window` and `deno.ns`; `deno.unstable` uses declarations
present in that CLI's output. Worker scopes and arbitrary runtime library
combinations are unsupported. The sidecar uses its bundled TypeScript compiler,
so this is not a replacement for `deno check` or a promise to support every
Deno runtime API.

Preparation writes only generated files under `.carrick/deno`. No source
`package.json` or user-maintained TypeScript config is required. Capture
rewrites resolved imports into the declaration tree, pins referenced npm
packages and scopes reachable runtime declarations to the producer. The
compatibility checker can then read the stubs without installing Deno.
If one captured tree references multiple versions of the same npm package,
capture fails explicitly because a single dependency pin cannot preserve both.

The Deno integration tests in `test/deno-project.test.ts` require Deno on
`PATH`; they report a skip when it is unavailable. Run them after building:

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
      "anchor_origin": "anchor-backfill"
    }
  ]
}
```

`anchor_origin` is one of `llm-symbol`, `deterministic-infer`, `anchor-backfill`.

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

#### `infer` - Resolve the type at a locator

Each item locates one expression. The fields are `file_path`, `line_number` and `infer_kind`; a locator is completed by a span (`span_start` + `span_end`), by `expression_text` (+ optional `expression_line`), or by the line alone for the kinds that anchor on a function (`function_return`, `signature_return`, `function_param`, `response_body`, `request_body`). Anything else is rejected per item, and that item alone pads to `unknown` — a bad item never sinks the batch.

`infer_kind` is one of `function_return`, `expression`, `call_result`, `variable`, `response_body`, `request_body`, `signature_return`, `function_param`, `receiver_type`.

`extraction_config` carries the caller's unwrap rules (wrapper symbols, origin module globs, payload paths). **Live behaviour depends on it**: without it the inferrer cannot unwrap a framework envelope, so a probe written without one does not reproduce what a real scan sees.

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
