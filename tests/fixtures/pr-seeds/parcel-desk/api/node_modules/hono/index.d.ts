export interface HonoRequest {
  raw: unknown;
  param(name: string): string;
  query(name: string): string | undefined;
  json<T = unknown>(): Promise<T>;
}

export interface Context {
  req: HonoRequest;
  json<T>(object: T, status?: number): Response;
  text(text: string, status?: number): Response;
}

export type Handler = (c: Context) => Response | Promise<Response>;

export declare class Hono {
  get(path: string, ...handlers: Handler[]): this;
  post(path: string, ...handlers: Handler[]): this;
  put(path: string, ...handlers: Handler[]): this;
  delete(path: string, ...handlers: Handler[]): this;
  all(path: string, ...handlers: Handler[]): this;
  fetch(request: unknown): Promise<Response>;
}
