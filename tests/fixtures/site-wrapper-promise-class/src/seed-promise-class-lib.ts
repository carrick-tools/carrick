export interface Schema<T> { parse(value: unknown): T }
export type Output<S> = S extends Schema<infer T> ? T : never;
export class CallPromise<T> extends Promise<T> {}
async function doFetch<S extends Schema<unknown>>(schema: S, url: string, init?: RequestInit): Promise<Output<S>> {
  const response = await fetch(url, init);
  const body = await response.json();
  return schema.parse(body) as Output<S>;
}
export function schemaFetch<S extends Schema<unknown>>(schema: S, url: string, init?: RequestInit): CallPromise<Output<S>> {
  return new CallPromise((resolve, reject) => doFetch(schema, url, init).then(resolve, reject));
}
export type QueryBody = { format: "json"; results: unknown[] } | { format: "csv"; results: string };
declare const QueryBodySchema: Schema<QueryBody>;
export class Api {
  constructor(private base: string) {}
  executeQuery(query: string): Promise<QueryBody> {
    return schemaFetch(QueryBodySchema, `${this.base}/api/v1/query`, { method: "POST", body: JSON.stringify({ query }) });
  }
}
export const api = new Api("/svc");
