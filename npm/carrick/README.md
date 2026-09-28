<p align="center"><a href="https://carrick.tools"><img src="https://carrick.tools/brand/carrick-banner@2x.png" alt="Carrick" width="100%"></a></p>

# Carrick

### TypeScript codebase intelligence for AI agents & IDEs.

Coding agents working across full-stack and multi-service codebases routinely rebuild existing helpers, trust stale type definitions, and modify API contracts without knowing who consumes them.

Carrick solves this by indexing your entire TypeScript ecosystem, from frontend apps to backend services, whether they live in a monorepo or across multiple repositories. By integrating deeply with the TypeScript compiler, Carrick traces every route, type, and cross-service call while recording function behaviour—so agents search by intent rather than name and stay grounded in your real architecture.

Delivered via MCP for AI agents and LSP for IDEs, Carrick ensures models see existing endpoints and utilities before generating new code. The scanner is source-available, runs from your CLI or CI pipeline, and includes a Free plan so you can start building safer agent workflows immediately.

## Get started

```bash
npm install -g carrick
carrick init
```

`carrick init` signs you in, asks which repositories to index and connects your agent. Your agent then writes each repository's `carrick.json` and runs the first scan. The [quick start](https://docs.carrick.tools/quickstart) has the details (Node 24 or newer).

## One index, three surfaces

Read indexed contracts in your editor, find existing code through your agent, and check changes on your pull request.

- **In your editor.** See an indexed counterpart, its contract, and the local file on the other side without leaving the editor.
- **In your agent.** Code that already exists, found by what it does, the real shape of any endpoint, the service graph, and who breaks if it changes.
- **In your pull request.** Contract risks, duplicated work and version drift across the indexed services.

## Ask your agent

- "Which functions handle webhook signing across our services?"
- "Where do we deduplicate users by email?"
- "What calls `/api/users`, and what response shape does each caller expect?"
- "Show me every function that retries on rate-limit errors."

## Docs

- [CLI reference](https://docs.carrick.tools/cli)
- [Building the index](https://docs.carrick.tools/building-the-index)
- [carrick.json](https://docs.carrick.tools/carrick-json)
- [MCP tools](https://docs.carrick.tools/mcp-tools)
- [In your editor](https://docs.carrick.tools/editor)

## Licence

Carrick has a free tier, and paid plans are on the [pricing page](https://carrick.tools/pricing). Elastic License 2.0. See
[LICENSE.md](https://github.com/carrick-tools/carrick/blob/main/LICENSE.md).
