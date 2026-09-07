# TypeSidecar - Compiler-Based Type Extraction

The TypeSidecar is a warm-standby Node.js process that provides TypeScript type resolution capabilities to the Carrick analysis engine. It uses the full TypeScript compiler (via `ts-morph`) to extract and bundle types, enabling accurate type checking across repositories.

## Overview

### Why a Sidecar?

The sidecar architecture solves several problems with the previous position-based type extraction:

1. **Accuracy**: Uses TypeScript's compiler for type resolution instead of fragile UTF-16 position calculations
2. **Type Inference**: Can extract types even when developers don't provide explicit annotations
3. **Parallel Startup**: Spawns immediately at CLI start; initializes while SWC scanning proceeds
4. **Warm Standby**: Process stays alive between requests for fast subsequent queries (~50ms)

### Capabilities

- **Symbol-Based Resolution**: Resolve type symbols by name (e.g., `User`, `Response<Order[]>`)
- **Type Inference**: Infer types at specific file locations using TypeScript's inference engine
- **Dependency Bundling**: Generate flat `.d.ts` files with all transitive dependencies
- **Manifest Generation**: Track which endpoints map to which types

## Building

```bash
# From the sidecar directory
cd src/sidecar

# Install dependencies
npm install

# Build TypeScript
npm run build

# Run tests
npm test
```

The build output goes to `dist/` directory.

## Usage

### From Rust

The sidecar is managed by the `TypeSidecar` struct in `src/services/type_sidecar.rs`:

```rust
use crate::services::type_sidecar::TypeSidecar;

// Spawn sidecar at CLI startup (non-blocking)
let sidecar = TypeSidecar::spawn(&sidecar_path)?;

// Wait for initialization
sidecar.wait_for_init(&repo_root, None)?;

// Resolve types
let result = sidecar.resolve_all_types(&explicit_symbols, &infer_requests, &[])?;
```

### Standalone Testing

```bash
# Start the sidecar
node dist/index.js

# Send JSON requests via stdin (one per line)
{"request_id": "1", "action": "init", "repo_root": "/path/to/repo"}
{"request_id": "2", "action": "bundle", "symbols": [{"name": "User", "source_file": "src/types.ts"}]}
{"request_id": "3", "action": "shutdown"}
```

## Message Protocol

Communication uses JSON over stdio:
- **stdin**: JSON requests (one per line)
- **stdout**: JSON responses (one per line)
- **stderr**: Log messages (for debugging)

### Actions

#### `init` - Point the sidecar at a project

Resolves which tsconfig (or default patterns) the project will be built from
and answers immediately. The ts-morph project itself is built by the first
request that reads it — `bundle`, `emit_surface`, `infer` or
`resolve_definitions` — and reused after that; `capture_v2` and `check_v2` are
stateless and build their own. So `init` costs the same on a repo with its
dependencies installed as on a bare checkout, and the time a large program
takes to build is charged to a request's deadline rather than to readiness
(carrick#749).

Re-initialising re-scopes the sidecar to another root, and drops the previous
project along with everything built over it.

```json
{
  "request_id": "unique-id",
  "action": "init",
  "repo_root": "/absolute/path/to/repo",
  "tsconfig_path": "tsconfig.json"  // optional, relative to repo_root
}
```

Response:
```json
{
  "request_id": "unique-id",
  "status": "ready",
  "init_time_ms": 523
}
```

#### `bundle` - Bundle Explicit Types

```json
{
  "request_id": "unique-id",
  "action": "bundle",
  "symbols": [
    {
      "name": "User",
      "source_file": "src/types/user.ts",
      "endpoint_method": "GET",
      "endpoint_path": "/api/users/:id",
      "is_producer": true
    }
  ]
}
```

Response:
```json
{
  "request_id": "unique-id",
  "status": "success",
  "dts_content": "export interface User { id: string; name: string; }",
  "manifest": [
    {
      "method": "GET",
      "path": "/api/users/:id",
      "is_producer": true,
      "type_alias": "User",
      "source_file": "src/types/user.ts"
    }
  ],
  "symbol_failures": []
}
```

#### `infer` - Infer Implicit Types

```json
{
  "request_id": "unique-id",
  "action": "infer",
  "requests": [
    {
      "source_file": "src/routes/users.ts",
      "line": 25,
      "kind": "handler_return",
      "endpoint_method": "GET",
      "endpoint_path": "/api/users"
    }
  ]
}
```

Response:
```json
{
  "request_id": "unique-id",
  "status": "success",
  "inferred_types": [
    {
      "source_file": "src/routes/users.ts",
      "line": 25,
      "inferred_type": "User[]",
      "dts_content": "export interface User { id: string; name: string; }",
      "endpoint_method": "GET",
      "endpoint_path": "/api/users"
    }
  ]
}
```

#### `health` - Check Status

```json
{
  "request_id": "unique-id",
  "action": "health"
}
```

Response:
```json
{
  "request_id": "unique-id",
  "status": "ready",
  "init_time_ms": 523
}
```

#### `shutdown` - Graceful Exit

```json
{
  "request_id": "unique-id",
  "action": "shutdown"
}
```

Response:
```json
{
  "request_id": "unique-id",
  "status": "success"
}
```

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                       TypeSidecar (Node.js)                      │
│                                                                  │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────────┐   │
│  │ ProjectLoader │    │ TypeBundler  │    │ TypeInferrer     │   │
│  │              │    │              │    │                  │   │
│  │ - Load tsconfig   │ - Resolve symbols  │ - Infer at location  │
│  │ - Init ts-morph   │ - Bundle .d.ts     │ - Extract types      │
│  │ - Validate        │ - Generate manifest│ - Handle handlers    │
│  └──────────────┘    └──────────────┘    └──────────────────┘   │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │                    Message Loop (index.ts)                │   │
│  │                                                           │   │
│  │  stdin ──► JSON parse ──► Route ──► Handle ──► stdout     │   │
│  └──────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

## Files

| File | Description |
|------|-------------|
| `src/index.ts` | Main entry point, message loop |
| `src/types.ts` | TypeScript interfaces for requests/responses |
| `src/validators.ts` | Zod schemas for request validation |
| `src/project-loader.ts` | TypeScript project initialization |
| `src/bundler.ts` | Type bundling with dts-bundle-generator |
| `src/type-inferrer.ts` | Type inference using TypeScript compiler |
| `test/` | Integration tests |

## Error Handling

All responses include a `status` field:
- `"ready"` - Sidecar initialized and ready
- `"success"` - Request completed successfully
- `"error"` - Request failed (check `errors` array)
- `"not_ready"` - Sidecar not yet initialized

Error responses include an `errors` array with details:
```json
{
  "request_id": "unique-id",
  "status": "error",
  "errors": ["Symbol 'Foo' not found in src/types.ts"]
}
```

## Performance

- **Cold start**: ~500ms (TypeScript compiler initialization)
- **Warm requests**: ~50ms per batch
- **Memory**: ~100-200MB depending on project size

The parallel startup strategy ensures the sidecar is ready by the time type resolution is needed:

```
CLI Start ────┬──► Spawn Sidecar (init in background)
              │
              ├──► SWC Scan Files (~100ms)
              │
              ├──► LLM Analysis (~2-5s)
              │
              └──► Type Resolution (sidecar now ready)
```

## Debugging

Enable verbose logging by checking stderr output:

```bash
node dist/index.js 2>&1 | tee sidecar.log
```

Log format:
```
[sidecar] Process started
[sidecar] Initializing with repo_root: /path/to/repo
[sidecar] Initialization complete in 523ms
[sidecar:error] Symbol not found: Foo
```

## See Also

- `src/services/type_sidecar.rs` - Rust client for the sidecar
- `docs/archive/compiler-sidecar-architecture/ARCHITECTURE.md` - Full architecture documentation
- `docs/archive/compiler-sidecar-architecture/IMPLEMENTATION_PLAN.md` - Implementation plan
