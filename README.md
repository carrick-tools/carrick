<p align="center"><a href="https://carrick.tools"><img src="https://carrick.tools/brand/carrick-banner@2x.png" alt="Carrick" width="100%"></a></p>

# Carrick

### TypeScript codebase intelligence for AI agents & IDEs.

[![Listed on mcpservers.org](https://mcpservers.org/badge.svg)](https://mcpservers.org/servers/carrick-tools/carrick) [![GitHub Marketplace](https://img.shields.io/badge/GitHub%20Marketplace-Action-7a55e8)](https://github.com/marketplace/actions/carrick-typescript-context-engine) [![Carrick MCP connector – tool definition quality and endpoint health on Glama](https://glama.ai/mcp/connectors/io.github.carrick-tools/carrick/badges/score.svg)](https://glama.ai/mcp/connectors/io.github.carrick-tools/carrick) [![Claude directory](https://img.shields.io/badge/Claude%20directory-connector-7a55e8)](https://claude.ai/directory/connectors/carrick) [![npm](https://img.shields.io/npm/v/carrick?color=7a55e8)](https://www.npmjs.com/package/carrick) [![Open VSX](https://img.shields.io/badge/Open%20VSX-extension-7a55e8)](https://open-vsx.org/extension/carrick-tools/carrick) [![MCP Registry](https://img.shields.io/badge/MCP%20Registry-listed-7a55e8)](https://registry.modelcontextprotocol.io/v0/servers?search=io.github.carrick-tools/carrick)

Coding agents working across full-stack and multi-service codebases routinely rebuild existing helpers, trust stale type definitions, and modify API contracts without knowing who consumes them.

Carrick solves this by indexing your entire TypeScript ecosystem, from frontend apps to backend services, whether they live in a monorepo or across multiple repositories. By integrating deeply with the TypeScript compiler, Carrick traces every route, type, and cross-service call while recording function behaviour—so agents search by intent rather than name and stay grounded in your real architecture.

Delivered via MCP for AI agents and LSP for IDEs, Carrick ensures models see existing endpoints and utilities before generating new code. The scanner is source-available, runs from your CLI or CI pipeline, and includes a Free plan so you can start building safer agent workflows immediately.

## Get started

```bash
npm install -g carrick
carrick init
```

`carrick init` signs you in, asks which repositories to index and connects your agent. Your agent then writes each repository's `carrick.json` and runs the first scan. The [quick start](https://docs.carrick.tools/quickstart) has the details (Node 24 or newer).

## Know the whole codebase before you change it

### Grep by meaning, not by name

About half the time an agent completes a task, it has quietly rewritten something the codebase already had. Grep only finds the name it guessed. Carrick maps every function to its intent, so agents find the implementation that exists and build on it.

### Cross-boundary type safety

The compiler's type safety stops at the service boundary. It can't see the service on the other side of a fetch. Carrick resolves request and response types across that boundary, mapping routing topology and middleware chains, so contract drift surfaces without a dedicated test suite.

### Skills for impact, reuse, drift and census

Carrick ships with four skills for the most common agent failures — breaking a caller in another service, rebuilding existing code, trusting a stale copy of another service's type, and missing places in a codebase-wide change. Each skill gets its answer from the Carrick index.

### Go to definition, across the service boundary

The same index, inside the editor. Go to definition jumps from an indexed call to its handler when the other service's code is on disk. A route's code lens lists the counterpart call sites held in the local index. Type disagreements appear as diagnostics on the indexed route or call.

### Catch contract drift in the pull request

The same index your agent uses runs in CI. When a producer and consumer drift apart, whichever protocol they speak, Carrick flags the mismatch in the PR before it merges. It also flags dependency version conflicts across services.

### The workspace

Every service, type and contract in one index, browsable in the dashboard and served to your agents over MCP.

## Ask your agent

- "Which functions handle webhook signing across our services?"
- "Where do we deduplicate users by email?"
- "What calls `/api/users`, and what response shape does each caller expect?"
- "Show me every function that retries on rate-limit errors."

## MCP tools

The MCP endpoint exposes the index as structured tools your agent can call directly. The [MCP tools reference](https://docs.carrick.tools/mcp-tools) lists each tool's parameters and response.

| Tool | Purpose |
| :--- | :--- |
| `search_by_intent` | Search functions by what they do, matching a plain-English query against each function's intent |
| `find_similar` | Find similar functions, for code you are about to write or duplicates already in the project |
| `list_function_intents` | Browse indexed functions and intents, a page at a time, by service or file |
| `get_project_map` | Show the project map, a short summary of services, contracts and unmatched calls |
| `list_projects` | List workspace projects and the repositories connected to each |
| `list_services` | List the project's services, with their endpoint, call and function counts |
| `get_service_graph` | Show the cross-service call graph, with unmatched calls and orphaned endpoints |
| `get_operation` | Find who serves and calls an operation, given its method and path |
| `get_callers` | Find callers of a function anywhere in the project |
| `get_api_endpoints` | List a service's operations across HTTP, GraphQL, WebSockets and pub/sub |
| `get_endpoint_types` | Read an endpoint's request and response types as the TypeScript compiler resolved them |
| `get_type_definition` | Resolve a named type definition to its expanded TypeScript |
| `check_compatibility` | Check a consumer against a producer, with a type verdict for each call |
| `get_service_dependencies` | Read npm dependencies and version conflicts, for one service or across the project |
| `list_external_calls` | List outbound calls to external targets, such as SDK calls, external domains and env-var URLs |
| `get_contract_pair` | Compare consumer and producer types, operation by operation |
| `scaffold` | Generate Carrick setup files (the workflow, a Carrick skill, `carrick.json` and Claude Code hooks) |

## Keep the index current

The GitHub Action refreshes each repository's part of the index on every push to main. It runs read-only in CI, with no API key to store.

```yaml
name: Carrick
on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
permissions:
  id-token: write
  contents: read
jobs:
  carrick:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: carrick-tools/carrick@v1
```

[Building the index](https://docs.carrick.tools/building-the-index) has the full workflow, with on-demand rescans, dependency installs and monorepo setup.

## Docs

- [How Carrick works](https://docs.carrick.tools/how-it-works)
- [What Carrick covers](https://docs.carrick.tools/coverage)
- [carrick.json](https://docs.carrick.tools/carrick-json)
- [CLI](https://docs.carrick.tools/cli)
- [MCP tools](https://docs.carrick.tools/mcp-tools)
- [In your editor](https://docs.carrick.tools/editor)
- [PR comments](https://docs.carrick.tools/pr-output)

## Licence

Carrick has a free tier, and paid plans are on the [pricing page](https://carrick.tools/pricing). The scanner in this repository is source-available under the [Elastic License 2.0](LICENSE.md). Copyright (c) 2026 Far Harbour B.V.

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
