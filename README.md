# Carrick

Carrick is a live, type-aware, intent-aware cross-repo index of every TypeScript service in your GitHub org, exposed to AI coding agents over the Model Context Protocol.

> Carrick is TypeScript only. Any `package.json`-based project works, whichever package manager you use — npm, pnpm, Yarn, or Bun. Carrick reads the manifest and never runs your install, so it needs no lockfile and no `node_modules`. Deno-native projects (no `package.json`) aren't supported yet. Cross-repo features need at least two services indexed in the same GitHub org; a single-service install still gets same-repo validation.

**Get started:** sign up at [app.carrick.tools](https://app.carrick.tools) · full documentation at [docs.carrick.tools](https://docs.carrick.tools)

## What an agent can ask

Connect Claude Code, Cursor, Windsurf, or Codex to the Carrick MCP endpoint. Carrick answers semantic questions about your org that an agent normally has to grep across repos to answer, badly:

- "Which functions handle webhook signing across our services?"
- "Where do we deduplicate users by email?"
- "What calls `/api/users` and what response shape do they expect?"
- "Show me every function that retries on rate-limit errors."

These work because the index combines structural facts, resolved types, and a per-function description of what the code actually does.

## What's in the index

For every scanned function in every repo in your org, Carrick stores three layers:

- **Structural.** Endpoints declared, outbound calls made, mounts, normalised paths.
- **Type-aware.** Request and response types resolved through the TypeScript compiler, so cross-repo type compatibility is checkable.
- **Intent-aware.** A one or two sentence description of what each function does, generated at scan time and stored alongside the structural and type data.

The intent layer is the difference. It is what lets an agent answer "where do we deduplicate users by email" rather than "which functions are named `dedupeUser`."

## Connect your agent

The MCP endpoint lives at `https://api.carrick.tools/mcp`.

```bash
claude mcp add --scope user --transport http carrick https://api.carrick.tools/mcp
```

The recommended authentication is sign-in-with-Carrick: your agent opens a browser, you click Approve once, and no API key changes hands. A manual key paste is available as a fallback. To get started, sign up at [app.carrick.tools](https://app.carrick.tools) — the full setup guide lives at [docs.carrick.tools](https://docs.carrick.tools).

## Populate the index

The index is populated by running the Carrick GitHub Action on each TypeScript repo you want indexed. On the main branch the action refreshes that repo's contribution to the index. On pull requests the Carrick App posts a drift comment for you (no extra workflow steps required).

```yaml
name: Carrick

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
  # Lets Carrick re-trigger this repo's main scan when a sibling repo in the
  # project changes. Optional today and dormant unless enabled server-side —
  # included here so it's already wired if you ever turn it on.
  repository_dispatch:
    types: [carrick-sibling-updated]

permissions:
  id-token: write
  contents: read

jobs:
  carrick:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        # Full git history lets Carrick diff against the last scan and run incrementally.
        with:
          fetch-depth: 0

      - uses: carrick-tools/carrick@v1
```

No secrets required. The `id-token: write` permission lets the action mint a short-lived GitHub Actions OIDC token, which Carrick uses to verify the repo's identity and authorize the upload. On pull requests the Carrick App posts the drift comment itself, so the workflow needs no extra permissions and no comment-posting step. Just make sure the Carrick GitHub App is installed on the org and the repo is connected to a project in the dashboard.

Pull requests opened from forks are skipped gracefully: GitHub withholds OIDC credentials from fork runs, so the action prints a notice and exits successfully instead of failing the check. The scan runs when a maintainer pushes the branch to the repository itself.

## MCP tools

The MCP endpoint exposes the index as structured tools your agent can call directly.

| Tool | Purpose |
| :--- | :--- |
| `search_by_intent` | Find functions by what they do — a plain-English query matched against the intent descriptions |
| `list_projects` | The Carrick projects in your workspace and each project's connected repos |
| `list_services` | Every service Carrick has indexed in your org |
| `list_function_intents` | One or two sentence descriptions of indexed functions, searchable by service |
| `get_api_endpoints` | Endpoints declared by a given service |
| `get_endpoint_types` | Resolved request and response types for a specific endpoint |
| `get_type_definition` | Fully resolved TypeScript type by name, across the org |
| `get_service_dependencies` | Services that call a given producer |
| `check_compatibility` | Whether service A's call to service B matches the producer's contract |
| `scaffold` | Generates the files to onboard a repo: the GitHub Actions workflow, an agent guide, and a `carrick.json` skeleton |

## On pull requests

On pull requests the Carrick App posts a comment summarising drift detected against the indexed services: type mismatches between producers and consumers, mismatched HTTP verbs, missing or orphaned routes, and npm-dependency-version conflicts. It updates the same comment in place on each push to the PR. PR comments are on by default for new projects and can be toggled per project in the dashboard; PR runs never alter the index.

## Configuration

Add a `carrick.json` to each indexed service to help classify outbound calls.

```json
{
  "serviceName": "order-service",
  "internalEnvVars": ["USER_SERVICE_URL", "INVENTORY_API"],
  "externalEnvVars": ["STRIPE_API", "GITHUB_API"],
  "internalDomains": ["https://api.yourcompany.com"],
  "externalDomains": ["https://api.stripe.com", "https://api.github.com"]
}
```

| Field | Description |
| :--- | :--- |
| `serviceName` | Friendly name for this service |
| `internalEnvVars` | Env vars pointing at other services in your org. Calls are validated against the index. |
| `externalEnvVars` | Env vars pointing at third-party APIs. Calls are ignored. |
| `internalDomains` | Full URL prefixes for internal services |
| `externalDomains` | Full URL prefixes for third-party APIs to ignore |

When Carrick sees a call like `fetch(process.env.ORDER_SERVICE_URL + '/orders')`, it needs to know whether `ORDER_SERVICE_URL` points internally or externally. Unclassified env vars surface as a configuration suggestion in the PR comment.

### Monorepos

`carrick.json` is optional — with no config (or a flat config like above) Carrick scans the repo as a single service. To index several services from one repository (e.g. a set of lambdas plus a dashboard), declare them with a `services` array instead. Each entry is scanned independently and indexed as its own service:

```json
{
  "includes": {
    "lambdas/_shared": {
      "externalEnvVars": ["GITHUB_API_BASE"],
      "externalDomains": ["https://api.github.com"]
    }
  },
  "services": [
    {
      "name": "check-or-upload",
      "directory": "lambdas/check-or-upload",
      "include": ["lambdas/_shared"],
      "internalEnvVars": ["CARRICK_API_ENDPOINT"]
    },
    {
      "name": "dashboard",
      "directory": "app",
      "tsconfig": "tsconfig.json"
    }
  ]
}
```

| Field | Description |
| :--- | :--- |
| `name` | Service name (alias for `serviceName` inside a `services` entry) |
| `directory` | Service root, relative to `carrick.json`. Files outside every declared directory are ignored |
| `include` | Extra source roots to pull in for type/function resolution (e.g. shared libraries copied in at build time), relative to `carrick.json` |
| `tsconfig` | Path to this service's `tsconfig.json`, relative to `directory`. Scopes type extraction to the service |

Alongside `services`, the optional top-level `includes` map declares classification for a shared source root once. See [Declaring a shared root once](#declaring-a-shared-root-once).

Each service also accepts the call-classification fields (`internalEnvVars`, `externalEnvVars`, `internalDomains`, `externalDomains`). When `services` is present, any sibling top-level flat fields are ignored. Cross-service drift, dependency conflicts, and duplicate intents are detected between the declared services just as they are across repositories.

#### Declaring a shared root once

When several services reach a third-party API through the same shared directory, the calls belong to that directory, not to each service that pulls it in. The optional top-level `includes` map declares classification per shared source root:

```json
{
  "includes": {
    "lambdas/_shared": {
      "externalEnvVars": ["GITHUB_API_BASE"],
      "externalDomains": ["https://api.github.com"]
    }
  }
}
```

Each key is a source root spelled as a service names it in its `include` (a leading `./` and a trailing `/` are ignored when matching). Each value takes the same four classification fields a service takes.

Every service whose `include` lists that root inherits those declarations, unioned with its own. A service keeps everything it declares itself, and a name declared in both places appears once. A service that does not include the root inherits nothing.

A key that no service lists in its `include` fails the scan rather than doing nothing, so a mistyped root is reported instead of leaving the calls unclassified.

## How it works

1. SWC parses each TypeScript file into an AST.
2. A static-analysis pass extracts function exports, mounted routers, pattern-matched HTTP calls, GraphQL schemas and operations, and WebSocket event contracts.
3. An LLM agent handles the cases pattern matching can't reach: dynamic URLs, factory functions, framework-specific routing.
4. A TypeScript sidecar resolves request and response types against the actual TypeScript compiler.
5. A second LLM pass writes the per-function intent description.
6. The org index lives in DynamoDB and S3 and refreshes each time a service's main branch runs.

## License

[Elastic License 2.0](LICENSE.md). Copyright (c) 2026 Far Harbour B.V.

## Development

See [AGENTS.md](AGENTS.md) for build, test, and contribution conventions.

```bash
cargo test
cargo fmt
cargo clippy
```

Install the optional pre-commit hook to run formatting and tests before each commit:

```bash
./scripts/install-hooks.sh
```
