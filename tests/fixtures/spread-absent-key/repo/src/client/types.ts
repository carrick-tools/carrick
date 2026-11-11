import type { HttpMethod } from '../core/types';

export type Config = {
  baseUrl?: string;
  fetch?: typeof fetch;
  headers?: Record<string, string>;
  parseAs?: 'auto' | 'json' | 'text' | 'blob';
  throwOnError?: boolean;
  requestValidator?: (options: unknown) => Promise<void>;
  bodySerializer?: (body: unknown) => string;
};

export type RequestOptions = Config & {
  url: string;
  method?: string;
  body?: unknown;
  path?: Record<string, string | number>;
  query?: Record<string, string>;
  security?: ReadonlyArray<{ scheme: string; type: string }>;
  signal?: AbortSignal;
  client?: Client;
};

export type MethodFn = (options: Omit<RequestOptions, 'method'>) => Promise<{ data?: unknown; error?: unknown }>;

export type Client = { [K in HttpMethod]: MethodFn } & {
  getConfig: () => Config;
  setConfig: (config: Config) => Config;
  request: (options: RequestOptions) => Promise<{ data?: unknown; error?: unknown }>;
  interceptors: Interceptors;
};

export type Interceptors = {
  request: { fns: Array<((request: Request, options: unknown) => Promise<Request>) | null> };
  response: { fns: Array<((response: Response, request: Request, options: unknown) => Promise<Response>) | null> };
};
