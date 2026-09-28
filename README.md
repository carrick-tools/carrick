<p align="center"><a href="https://carrick.tools"><img src="https://carrick.tools/brand/carrick-banner@2x.png" alt="Carrick" width="100%"></a></p>

# Carrick

### TypeScript codebase intelligence for AI agents & IDEs.

[![Listed on mcpservers.org](https://mcpservers.org/badge.svg)](https://mcpservers.org/servers/carrick-tools/carrick) [![GitHub Marketplace](https://img.shields.io/badge/GitHub%20Marketplace-Action-7a55e8)](https://github.com/marketplace/actions/carrick-typescript-context-engine) [![Carrick MCP connector – tool definition quality and endpoint health on Glama](https://glama.ai/mcp/connectors/io.github.carrick-tools/carrick/badges/score.svg)](https://glama.ai/mcp/connectors/io.github.carrick-tools/carrick) [![Claude directory](https://img.shields.io/badge/Claude%20directory-connector-7a55e8)](https://claude.ai/directory/connectors/carrick) [![npm](https://img.shields.io/npm/v/carrick?color=7a55e8)](https://www.npmjs.com/package/carrick) [![Open VSX](https://img.shields.io/badge/Open%20VSX-extension-7a55e8)](https://open-vsx.org/extension/carrick-tools/carrick) [![MCP Registry](https://img.shields.io/badge/MCP%20Registry-listed-7a55e8)](https://registry.modelcontextprotocol.io/v0/servers?search=io.github.carrick-tools/carrick)

Coding agents working across full-stack and multi-service codebases routinely rebuild existing helpers, trust stale type definitions, and modify API contracts without knowing who consumes them.

Carrick solves this by indexing your entire TypeScript ecosystem—from frontend apps to backend services—whether in a monorepo or across multiple repositories. By integrating deeply with the TypeScript compiler, Carrick traces every route, type, and cross-service call while recording function behavior—so agents search by intent rather than name and stay grounded in your real architecture.

Delivered via MCP for AI agents and LSP for IDEs, Carrick ensures models see existing endpoints and utilities before generating new code. The scanner is source-available, runs from your CLI or CI pipeline, and includes a Free plan so you can start building safer agent workflows immediately.

## Get started

```bash
npm install -g carrick
carrick init
```

`carrick init` signs you in, asks which repositories to index and connects your agent. Your agent then writes each repository's `carrick.json` and runs the first scan. The [quick start](https://docs.carrick.tools/quickstart) has the details (Node 24 or newer).

## One index, three surfaces

- **Your agent, over MCP.** Claude Code, Cursor, Windsurf, Codex and Claude search every function by what it does, across all your services, whether they are checked out or not. They read an endpoint's request and response types as the TypeScript compiler resolved them, and see who calls a route before they change it.
- **Your editor.** The extension shows broken contracts in the Problems panel, and jumps from a call to the handler that serves it.
- **Your pull requests.** The Carrick GitHub App comments when a change breaks a contract that another service depends on.

## Ask your agent

- "Which functions handle webhook signing across our services?"
- "Where do we deduplicate users by email?"
- "What calls `/api/users`, and what response shape does each caller expect?"
- "Show me every function that retries on rate-limit errors."

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
