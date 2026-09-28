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

Search existing code through your agent, inspect live contracts in your editor, and catch breaking changes in your pull requests.

### In your agent

Give your AI assistants complete context on what already exists, how endpoints are shaped, and who breaks if something changes. Carrick maps every function by what it actually does, allowing agents to find existing implementations and build on them—even in repositories you haven't checked out locally. Endpoint types are pulled straight from the TypeScript compiler in the service that exposes them.

Works with any MCP-compatible agent, including Claude Code, Cursor, Windsurf, and Codex.

### In your editor

Inspect cross-service contracts and jump between connected files without leaving your IDE. "Go to Definition" jumps directly from a service call to its handler when the code is on disk, while Code Lens displays every caller across your local index inline. If a frontend call and backend handler fall out of sync, Carrick surfaces type mismatches as native editor diagnostics.

Available on the Visual Studio Marketplace and Open VSX, or runnable as a standard LSP server.

### In your pull request

Bring the exact same index into CI to catch contract risks, duplicate work, and silent version drift. When a producer and consumer fall out of sync—regardless of protocol—Carrick flags the exact mismatch directly on your PR before it merges. It also checks dependency versions across services to catch conflicting package updates early.

## Skills

Carrick ships with four skills for the most common agent failures — breaking a caller in another service, rebuilding existing code, trusting a stale copy of another service's type, and missing places in a codebase-wide change. Each skill gets its answer from the Carrick index. `carrick init` installs them into `.claude/skills/` and `.agents/skills/`.

| Skill | Use it | What it returns |
| :--- | :--- | :--- |
| [`carrick-impact`](https://docs.carrick.tools/carrick-impact) | Before changing or deleting a route, handler, response shape, event or shared function, or to ask "who calls this?" | Everything that depends on the code you are about to change, with a file and line for each, and a type verdict for each consumer |
| [`carrick-reuse`](https://docs.carrick.tools/carrick-reuse) | At the end of a task that added or changed functions, or to ask "does this already exist?" | The new functions compared against the whole function index, and the places the project has built the same thing twice |
| [`carrick-drift`](https://docs.carrick.tools/carrick-drift) | Before changing a request or response type, or when a compatibility verdict names a problem you cannot place | The producer's type, each consumer call site's expected type and the stored verdict, side by side, one operation at a time |
| [`carrick-census`](https://docs.carrick.tools/carrick-census) | For "find every place that does X" questions | Every match for two wordings of one concept, paged to the end and joined into one list with a receipt |

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
- [Task skills](https://docs.carrick.tools/task-skills)
- [In your editor](https://docs.carrick.tools/editor)

## Licence

Carrick has a free tier, and paid plans are on the [pricing page](https://carrick.tools/pricing). Elastic License 2.0. See
[LICENSE.md](https://github.com/carrick-tools/carrick/blob/main/LICENSE.md).
