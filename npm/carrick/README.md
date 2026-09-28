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

Find existing code through your agent, read indexed contracts in your editor, and check changes on your pull request.

### In your agent

Code that already exists, found by what it does, the real shape of any endpoint, the service graph, and who breaks if it changes. Carrick maps every function to its intent, so agents find the implementation that exists and build on it, including in repositories that are not checked out. Endpoint types come from the TypeScript compiler in the service that serves them. Agents that support MCP can connect to Carrick, including Claude Code, Cursor, Windsurf and Codex.

### In your editor

See an indexed counterpart, its contract, and the local file on the other side without leaving the editor. Go to definition jumps from an indexed call to its handler when the other service's code is on disk. A route's code lens lists the counterpart call sites held in the local index, and type disagreements appear as diagnostics on the indexed route or call. The editor extension is available from Visual Studio Marketplace and Open VSX, and other LSP clients can start the same server directly.

### In your pull request

Contract risks, duplicated work and version drift across the indexed services. The same index your agent uses runs in CI. When a producer and consumer drift apart, whichever protocol they speak, Carrick flags the mismatch in the PR before it merges. It also flags dependency version conflicts across services.

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
