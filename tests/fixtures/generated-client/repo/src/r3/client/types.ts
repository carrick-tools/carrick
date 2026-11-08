// Rung 3: the shape an OpenAPI generator writes into a repository.
export type HttpMethod = 'get' | 'post' | 'put' | 'delete';

export interface Config {
  baseUrl?: string;
  fetch?: typeof fetch;
  headers?: Record<string, string>;
  requestValidator?: (options: unknown) => Promise<void>;
}

export interface RequestOptions extends Config {
  url: string;
  method?: string;
  body?: unknown;
  path?: Record<string, string | number>;
  query?: Record<string, string>;
  client?: Client;
}

export type MethodFn = (options: Omit<RequestOptions, 'method'>) => Promise<{ data?: unknown; response: Response }>;

export type Client = { [K in HttpMethod]: MethodFn } & {
  request: (options: RequestOptions) => Promise<{ data?: unknown; response: Response }>;
  getConfig: () => Config;
  setConfig: (config: Config) => Config;
};
