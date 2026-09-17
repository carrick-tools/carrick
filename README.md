# Carrick

Carrick is a live, type-aware, intent-aware cross-repo index of every TypeScript service in your GitHub org, exposed to AI coding agents over the Model Context Protocol.

> Carrick scans TypeScript projects using npm, pnpm, Yarn, Bun, or Deno. Deno projects require Deno 2.9.4 or newer; the Action supplies the runtime and reads existing `deno.json` or `deno.jsonc` manifests. Dependency preparation disables lifecycle scripts ([details](#dependencies)). Cross-repo features need at least two services indexed in the same Carrick project; a single-service install still gets same-repo validation.

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

### Dependencies

Package types need their dependencies on disk. Node projects use installed `node_modules`, and Deno projects use the Deno dependency cache. The Action prepares those dependencies before analysis:

- For Node projects, installation runs when a lockfile sits at the path being analyzed. The lockfile picks the manager: `package-lock.json` runs `npm ci`, `pnpm-lock.yaml` runs `pnpm install --frozen-lockfile`, `yarn.lock` runs `yarn install`, `bun.lock`/`bun.lockb` runs `bun install`.
- Lifecycle scripts are disabled in every case, so nothing in your repo executes during a scan.
- Each install command has a five-minute timeout. A failed or timed-out install prints a warning, and the scan that follows refuses any service whose dependencies are still missing (see below).
- Existing `node_modules` skips the Node install. A Deno manifest at the scan root still triggers Deno cache preparation. In a monorepo preparation happens at the path being scanned.
- The package manager's download cache is restored between runs, keyed on the lockfile's hash.

For Deno roots the Action runs `deno install --frozen --node-modules-dir=none`.
This prepares the Deno dependency cache and prevents npm lifecycle scripts from
running even when the project authorizes them through `allowScripts`. A root
with both Node and Deno configuration prepares both dependency stores. Before
local indexing, install Deno 2.9.4 or newer and run the same preparation command
from the Deno workspace root. Projects that import generated declarations must
generate those declarations through their normal build before indexing.

### An unprepared checkout is refused

Carrick refuses to scan a checkout it cannot type, rather than charging for an
index whose types are `any` and saying nothing about why. The check runs before
the scan starts, per service, on what is reachable from that service's own
directory — a monorepo root that is installed says nothing about a nested
workspace that is not. Two things are refused:

- **Dependencies a lockfile states and the tree has not installed.** The
  refusal names the service and the exact command, because the lockfile names
  the package manager. A tree with no lockfile above the service states no
  install and is scanned as it is. Deno services are asked only when their
  config sets `nodeModulesDir`, since Deno otherwise caches outside the tree.
- **A config mapping whose target directory is not on the checkout.** A
  TypeScript `paths` entry, a `package.json` `imports` key or a Deno import-map
  entry pointing at, say, a generated client whose generator has not run. The
  refusal names the mapping and the missing directory and stops there: nothing
  in the config says what fills a generated directory.

Both are proxies, so there is always a way past: `--allow-unprepared` on the
command, `CARRICK_ALLOW_UNPREPARED=1` in the environment, or
`allow-unprepared: true` on the Action (which `install-dependencies: false`
already implies). A pipeline that scans a bare checkout deliberately keeps
working; it just says so.

Deno services normally omit `tsconfig` and use their nearest Deno manifest.
An explicit ordinary TypeScript config selects the TypeScript path. An explicit
Deno config must name that nearest manifest; `deno.json` takes precedence over
`deno.jsonc` when both exist. Import maps must be local files. Nested Deno roots
without a Deno manifest at the Action's scan root need dependency preparation
in their own workspace before the Carrick step.

Turn it off with:

```yaml
      - uses: carrick-tools/carrick@v1
        with:
          install-dependencies: false
```

Private registries use your own credentials. Carrick adds no auth of its own: put the token your `.npmrc` reads in the job's environment and the install step inherits it.

```yaml
      - uses: carrick-tools/carrick@v1
        env:
          NPM_TOKEN: ${{ secrets.NPM_TOKEN }}
```

### Re-analyzing everything

A scan reads the model once per changed file and reuses what it already has for
the rest, which is what makes a routine scan cheap. Occasionally the answers
themselves need redoing rather than the files: Carrick starts extracting
something it did not extract before, and the cache holds answers from before it
could. `full-scan` re-analyzes every file for one run.

Ask for it, rather than leaving it on. Wire it to the workflow's
`workflow_dispatch` input, which is what `carrick init` scaffolds:

```yaml
on:
  workflow_dispatch:
    inputs:
      full-scan:
        type: boolean
        default: false

jobs:
  carrick:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - uses: carrick-tools/carrick@v1
        with:
          full-scan: ${{ inputs.full-scan }}
```

Then run it from the Actions tab, or:

```bash
gh workflow run carrick.yml -f full-scan=true
```

On every other trigger the expression is empty and the incremental scan runs
exactly as before.

An ordinary scan indexes a commit once per scanner version, so a second run on
an unchanged commit stores nothing and says so. A full scan is the exception:
its answers replace what the index holds for that commit, because re-analyzing
everything is a statement that the stored answers were the stale part.

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

`carrick.json` is optional. Without it, Carrick derives services from npm or pnpm workspace manifests; a plain repo is one service. An explicit config takes precedence over derivation. To set service boundaries and shared source includes, declare a `services` array. Each entry is scanned independently and indexed as its own service:

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
      "serviceName": "check-or-upload",
      "directory": "lambdas/check-or-upload",
      "include": ["lambdas/_shared"],
      "internalEnvVars": ["CARRICK_API_ENDPOINT"]
    },
    {
      "serviceName": "dashboard",
      "directory": "app",
      "tsconfig": "tsconfig.json"
    }
  ]
}
```

| Field | Description |
| :--- | :--- |
| `serviceName` | Service name, and the key the index is written under: what a sibling repo's calls are matched against, and what `carrick status` and the hosted rows name. `name` is accepted as an alias inside a `services` entry |
| `directory` | Service root, relative to `carrick.json`. Files outside every declared directory are ignored |
| `include` | Extra source roots to pull in for type/function resolution (e.g. shared libraries copied in at build time), relative to `carrick.json` |
| `tsconfig` | Optional TypeScript config path, relative to `directory`. Deno services normally omit this field and use their nearest Deno manifest |
| `graphqlSchemas` | Printed GraphQL SDL files that define the operations this service serves, relative to `carrick.json`; globs allowed. See [Code-first GraphQL schemas](#code-first-graphql-schemas) |

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

#### Code-first GraphQL schemas

Carrick reads a GraphQL server's operations from SDL: `.graphql`/`.gql` files under the service's own directory and `gql` template literals. A schema built in code (Pothos, TypeGraphQL, Nexus) has no SDL in source, so its queries and mutations are not indexed until the service names the printed schema:

```json
{
  "services": [
    {
      "serviceName": "api",
      "directory": "apps/api",
      "graphqlSchemas": ["apps/api/dist/schema.graphql"]
    }
  ]
}
```

Each entry is a path relative to `carrick.json`, or a glob such as `packages/schema/generated/*.graphql`. The file can sit anywhere in the repository, including a build folder like `dist/` or another app's directory, as long as it is committed. Every `Query`, `Mutation` and `Subscription` field it defines is indexed as an operation that this service serves. A flat single-service `carrick.json` accepts the field at the top level.

An entry that matches no file, or a file that defines no root field, is reported as a warning in the scan output. When a service depends on a GraphQL library and serves HTTP routes but indexes no GraphQL schema fields, the scan output suggests this setting.

Once the schema's fields are known, Carrick also reads the modules that build the schema: files that call through a builder value created from a library the scan detects, including field modules that export nothing and only import the builder. Each such file is analysed with the schema's field list, and a field whose resolver is found there is indexed at the resolver's line rather than at the printed schema. A field with no located resolver stays at its schema line.

#### GraphQL documents for another team's API

A client's GraphQL documents are indexed as calls only when they are written against a schema this repository serves. Carrick attributes each document (a `.graphql`/`.gql` file, or one `gql` template) to the committed schema file that holds its root fields. A schema file counts as served when a service names it in `graphqlSchemas`, or when it sits under a service's directory and that service shows it serves a schema: it serves HTTP routes, or a resolver in its code is linked to one of the schema's fields. Any other committed schema file marks its documents as calls to an external API, and they are not indexed as calls. That includes a vendor schema a codegen step downloaded into `dist/`, or a copy committed inside a client app's own `src/`. The fields of a copy that sits under a service with no such evidence are not indexed as that service's operations, and the scan output names the file. The scan output names the schema file and counts the operations, so a schema that is served here but not declared can be added to `graphqlSchemas`.

Some documents are left out as well:
- a document whose fields no single schema holds;
- a document whose fields sit in both a served and an external schema, when the file's environment reads don't settle it. A file that reads only variables from `internalEnvVars` counts as internal, and one that reads only `externalEnvVars` counts as external.

A document whose fields appear in no schema the repository holds stays a call, because its server may be another repository in the project.

#### Where a GraphQL call is indexed

A document written in a `.graphql`/`.gql` file and compiled into a typed declaration (`OrdersDocument`) is sent from the code that passes that declaration to a client, such as `useQuery(OrdersDocument)`. Carrick indexes the operation's fields at each of those calls, resolving the import through relative paths, `tsconfig` path aliases and workspace packages. An operation that no call executes stays indexed at its line in the document file. A `gql` template written in source stays indexed where it is written.

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
