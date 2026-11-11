// Ambient stubs — keep types resolvable without npm install.
// Just enough surface for the call sites; the scanner reads AST structure.

declare module "ky" {
  interface KyResponse {
    json<T>(): Promise<T>;
  }
  interface Ky {
    get(url: string): KyResponse;
    post(url: string, opts?: { json?: unknown }): KyResponse;
  }
  const ky: Ky;
  export default ky;
}

declare module "graphql-tag" {
  export function gql(literals: TemplateStringsArray, ...args: unknown[]): unknown;
  export default gql;
}

declare module "graphql-request" {
  export class GraphQLClient {
    constructor(url: string);
    request<T = unknown>(document: unknown, variables?: Record<string, unknown>): Promise<T>;
  }
}

declare module "msw" {
  export interface HttpHandler {
    readonly __mswHandler: true;
  }
  type Resolver = () => unknown;
  export const http: {
    get(path: string, resolver: Resolver): HttpHandler;
    post(path: string, resolver: Resolver): HttpHandler;
  };
  export const HttpResponse: {
    json(body: unknown): unknown;
  };
}

// Ambient zod stub — keeps `z.infer` computable without npm install.
declare module "zod" {
  export interface ZodType<Output> {
    readonly _output: Output;
    parse(data: unknown): Output;
  }
  export namespace z {
    export type infer<T extends ZodType<any>> = T["_output"];
    export function object<S extends Record<string, ZodType<any>>>(
      shape: S
    ): ZodType<{ [K in keyof S]: S[K]["_output"] }>;
    export function string(): ZodType<string>;
    export function number(): ZodType<number>;
    export function boolean(): ZodType<boolean>;
  }
}
