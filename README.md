# Carrick

[![Listed on mcpservers.org](https://mcpservers.org/badge.svg)](https://mcpservers.org/servers/carrick-tools/carrick) [![GitHub Marketplace](https://img.shields.io/badge/GitHub%20Marketplace-Action-7a55e8)](https://github.com/marketplace/actions/carrick-typescript-context-engine) [![Carrick MCP connector – tool definition quality and endpoint health on Glama](https://glama.ai/mcp/connectors/io.github.carrick-tools/carrick/badges/score.svg)](https://glama.ai/mcp/connectors/io.github.carrick-tools/carrick) [![Claude directory](https://img.shields.io/badge/Claude%20directory-connector-7a55e8)](https://claude.ai/directory/connectors/carrick) [![npm](https://img.shields.io/npm/v/carrick?color=7a55e8)](https://www.npmjs.com/package/carrick) [![Open VSX](https://img.shields.io/badge/Open%20VSX-extension-7a55e8)](https://open-vsx.org/extension/carrick-tools/carrick) [![MCP Registry](https://img.shields.io/badge/MCP%20Registry-listed-7a55e8)](https://registry.modelcontextprotocol.io/v0/servers?search=io.github.carrick-tools/carrick)

Carrick maps your entire TypeScript codebase across services and repositories, giving AI agents full context on existing types, routes, and function behaviours over MCP before they write duplicate or breaking code.

When coding agents work across the services of a TypeScript codebase, the failures that matter are rebuilding a helper that already exists, trusting a stale local copy of another service's type, or changing a response nobody knew was consumed. A smarter model cannot fix what it cannot see, so Carrick gives the agent the same source of truth the compiler has, across every service, at the moment it decides.

## Get started

```bash
npm i -g carrick
carrick init
```

The [quick start](https://docs.carrick.tools/quickstart) covers the full setup (Node 24 or newer).

## Agents see the whole system before they write

- **Find what exists.** Agents search every indexed function across services and repositories by what it does, whatever it is called, including repositories that are not checked out, so they build on the helper that exists instead of writing a second one.
- **Build against the real contract.** An agent reads an endpoint's request and response types as the TypeScript compiler resolved them in the service that serves it.
- **Change a route with its consumers in view.** Before changing a route, an agent can see every service that calls it and check each consumer against the producer.
- **Catch breaks in review.** The Carrick App comments on pull requests to flag cross-repo contract breaks introduced by the change, such as a changed response type that no longer matches a consumer in another repository.

Claude Code, Cursor, Windsurf, Codex and Claude (as a connector in the Claude directory) all connect to one MCP endpoint, `https://api.carrick.tools/mcp`, with GitHub sign-in, and the Carrick editor extension reads the same index in your editor. The index is built by the `carrick` CLI from a laptop or by the Carrick GitHub Action, which runs read-only in CI and refreshes each repository's part of the index on every push to its main branch. There is a free tier, and the scanner in this repository is source-available under the [Elastic License 2.0](LICENSE.md).

## Example questions

- "Which functions handle webhook signing across our services?"
- "Where do we deduplicate users by email?"
- "What calls `/api/users`, and what response shape does each caller expect?"
- "Show me every function that retries on rate-limit errors."

Each answer comes from one index that holds each service's routes and outbound calls, their request and response types as resolved by the TypeScript compiler, and a short description of what every indexed function does.

## Populate the index

The first index is built by `carrick index`. After that, the index is kept current by running the Carrick GitHub Action on each TypeScript repo you want indexed. On the main (or master) branch the action refreshes that repo's contribution to the index. On pull requests the Carrick App posts a drift comment for you (no extra workflow steps required).

```yaml
name: Carrick

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
  # Lets Carrick re-trigger this repo's main scan when a sibling repo in the
  # same project changes on its main branch. It is enabled server-side but not
  # delivering yet. If you add deploy steps to this file, gate them on
  # github.event_name so a sibling change never deploys this repo.
  repository_dispatch:
    types: [carrick-sibling-updated]
  # Rescan on demand (Actions tab, or: gh workflow run carrick.yml). After a
  # Carrick release the index only refreshes on the next scan.
  workflow_dispatch:
    inputs:
      full-scan:
        description: >-
          Re-analyze every file instead of reusing the cached answer for a file
          that has not changed. Slower and costs a full analysis; ask for it
          when Carrick has started extracting something it did not extract
          before, so the cache holds answers from before it could.
        type: boolean
        default: false

permissions:
  # Mint a short-lived OIDC token Carrick exchanges for keyless upload auth
  # (no upload secret to configure).
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
        with:
          # Empty on every trigger but workflow_dispatch, which is the point:
          # a full re-analysis is asked for, never carried by a push.
          full-scan: ${{ inputs.full-scan }}
```

No secrets required. The `id-token: write` permission lets the action mint a short-lived GitHub Actions OIDC token, which Carrick uses to verify the repo's identity and authorize the upload. On pull requests the Carrick App posts the drift comment itself, so the workflow needs no extra permissions and no comment-posting step. Just make sure the Carrick GitHub App is installed on the org and the repo is connected to a project (`carrick init` or the dashboard).

Pull requests opened from forks are skipped gracefully. GitHub withholds OIDC credentials from fork runs, so the action prints a notice and exits successfully instead of failing the check. The scan runs when a maintainer pushes the branch to the repository itself.

### Dependencies

Package types need their dependencies on disk. Node projects use installed `node_modules`, and Deno projects use the Deno dependency cache. The Action prepares those dependencies before analysis:

- For Node projects, installation runs for every service the scan will visit, at that service's own nearest lockfile. A monorepo whose packages carry their own lockfiles is installed package by package; a workspace that hoists to one lockfile at its root is installed once. The lockfile picks the manager: `package-lock.json` runs `npm ci`, `pnpm-lock.yaml` runs `pnpm install --frozen-lockfile`, `yarn.lock` runs `yarn install`, `bun.lock`/`bun.lockb` runs `bun install` (add `oven-sh/setup-bun` first).
- Lifecycle scripts are disabled in every case.
- Each install command has a five-minute timeout. A failed or timed-out install prints a warning, and the scan that follows refuses any service whose dependencies are still missing (see below).
- Existing `node_modules` skips that service's Node install. A Deno manifest still triggers Deno cache preparation, which the separate Deno cache does not get from a Node install.
- The package managers' download caches are restored between runs, keyed on the hash of every lockfile the run installs from.

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
directory. A monorepo root that is installed says nothing about a nested
workspace that is not. Two things are refused:

- **Dependencies an npm, pnpm, Yarn or Deno lockfile states and the tree has
  not installed.** The refusal names the service and the exact command,
  because the lockfile names the package manager. A tree with no lockfile above the service states no
  install and is scanned as it is. Deno services are asked only when their
  config sets `nodeModulesDir`, since Deno otherwise caches outside the tree.
- **A config mapping whose target directory is not on the checkout, that the
  service imports through.** A TypeScript `paths` entry, a `package.json`
  `imports` key or a Deno import-map entry pointing at, say, a generated client
  whose generator has not run. The refusal names the mapping and the missing
  directory and stops there. Nothing in the config says what fills a generated
  directory. A mapping left behind by a deleted package, that nothing imports,
  is logged and scanned past.

Both are proxies. To scan anyway, pass `--allow-unprepared` on the command,
set `CARRICK_ALLOW_UNPREPARED=1` in the environment, or set
`allow-unprepared: true` on the Action (which `install-dependencies: false`
already implies). A pipeline that scans a bare checkout deliberately keeps
working, with a warning.

Deno services normally omit `tsconfig` and use their nearest Deno manifest.
An explicit ordinary TypeScript config selects the TypeScript path. An explicit
Deno config must name that nearest manifest; `deno.json` takes precedence over
`deno.jsonc` when both exist. Import maps must be local files.

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
themselves need redoing rather than the files. That happens when Carrick starts
extracting something it did not extract before, and the cache holds answers
from before it could. `full-scan` re-analyzes every file for one run.

To ask for it on one run rather than leaving it on, wire it to the workflow's
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
an unchanged commit stores nothing and says so. A full scan, or any run that
re-analyses a file, is the exception. Its answers replace what the index holds
for that commit, because re-analysing is a statement that the stored answers
were the stale part.

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
| `externalEnvVars` | Env vars pointing at third-party APIs. Calls are not matched. They are listed as external calls. |
| `internalDomains` | Full URL prefixes for internal services |
| `externalDomains` | Full URL prefixes for third-party APIs to list as external calls rather than match |

When Carrick sees a call like `fetch(process.env.ORDER_SERVICE_URL + '/orders')`, it needs to know whether `ORDER_SERVICE_URL` points internally or externally. Unclassified env vars surface as a configuration suggestion in the PR comment.

### Monorepos

`carrick.json` is optional. Without it, Carrick derives services from npm, pnpm or Deno workspace manifests; a plain repo is one service. An explicit config takes precedence over derivation. To set service boundaries and shared source includes, declare a `services` array. Each entry is scanned independently and indexed as its own service:

```json
{
  "includes": {
    "packages/shared": {
      "externalEnvVars": ["STRIPE_API"],
      "externalDomains": ["https://api.stripe.com"]
    }
  },
  "services": [
    {
      "serviceName": "api",
      "directory": "apps/api",
      "include": ["packages/shared"],
      "internalEnvVars": ["USER_SERVICE_URL"]
    },
    {
      "serviceName": "web",
      "directory": "apps/web",
      "tsconfig": "tsconfig.json"
    }
  ]
}
```

| Field | Description |
| :--- | :--- |
| `serviceName` | Service name, and the key the index is written under. A sibling repo's calls are matched against it, and `carrick status` and the hosted rows name it. `name` is accepted as an alias inside a `services` entry |
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
    "packages/shared": {
      "externalEnvVars": ["STRIPE_API"],
      "externalDomains": ["https://api.stripe.com"]
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
2. A static-analysis pass extracts function exports, GraphQL schemas and operations, and WebSocket event contracts, and gates files with candidate routes or calls.
3. An LLM reads each gated file and extracts its routes, mounts and calls.
4. A TypeScript sidecar resolves request and response types against the actual TypeScript compiler.
5. A second LLM pass writes the per-function intent description.
6. The project index lives in DynamoDB and S3 and refreshes on each main-branch scan or `carrick index` run.

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
