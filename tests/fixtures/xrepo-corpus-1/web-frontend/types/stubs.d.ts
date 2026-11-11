// Ambient stubs — keep the GraphQL + WebSocket consumer types resolvable
// without npm install (mirrors the REST repos' *.d.ts stub convention).

// graphql-tag: the `gql` tagged-template the scanner keys documents on.
declare module "graphql-tag" {
  export function gql(
    literals: TemplateStringsArray,
    ...placeholders: unknown[]
  ): unknown;
}

// graphql-request: a minimal client used by lib/graphql.ts.
declare module "graphql-request" {
  export class GraphQLClient {
    constructor(url: string);
    request<T = unknown>(document: unknown, variables?: unknown): Promise<T>;
  }
}
