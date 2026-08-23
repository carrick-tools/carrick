/**
 * Minimal stand-ins for the two library shapes a schema-first route stack is
 * built from, declared locally so the fixture needs no installed packages
 * (same approach as `framework-handlers.ts`).
 *
 * Both mirror the real declarations the anchors read on a live service:
 *
 *  - a request object parameterised by a route generic, exposing the request
 *    body as its `body` member (`body: RouteGeneric['Body']`, defaulting to
 *    `unknown` when the route declares nothing);
 *  - a schema value that carries its parsed output type, reachable through the
 *    `parse` method every such value exposes, and a registry builder that maps
 *    names to schema values and hands back a `$ref`-style lookup function.
 */

export interface RouteGeneric {
  Body?: unknown;
  Params?: unknown;
  Query?: unknown;
}

export interface RouteRequest<Generic extends RouteGeneric = RouteGeneric> {
  body: Generic['Body'];
  params: Generic['Params'];
  query: Generic['Query'];
}

export interface Reply {
  status(code: number): Reply;
  send(payload?: unknown): Reply;
}

export interface RouteOptions {
  schema?: unknown;
}

export interface Server {
  post<Request extends RouteRequest = RouteRequest>(
    path: string,
    options: RouteOptions,
    handler: (request: Request, reply: Reply) => unknown
  ): void;
  get<Request extends RouteRequest = RouteRequest>(
    path: string,
    options: RouteOptions,
    handler: (request: Request, reply: Reply) => unknown
  ): void;
}

export interface Schema<Output> {
  readonly _output: Output;
  parse(data: unknown): Output;
}

/** The declared output type of a schema value (the `z.infer` of this stack). */
export type Infer<S> = S extends Schema<infer Output> ? Output : never;

export function defineSchema<Output>(): Schema<Output> {
  return {
    _output: undefined as unknown as Output,
    parse: (data: unknown) => data as Output,
  };
}

export function buildSchemaRefs<Models extends Record<string, Schema<unknown>>>(
  models: Models
): {
  names: string[];
  $ref: (key: keyof Models & string) => { $ref: string };
} {
  return {
    names: Object.keys(models),
    $ref: (key) => ({ $ref: `${key}#` }),
  };
}
