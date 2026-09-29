// `@fixture/http` as a release that renamed its base-URL option: `create`
// takes `baseUrl`, not the `baseURL` framework detection claims. Everything
// else is the same declaration. The test installs this over the vendored
// one, so the factory claim fails on the key while the instance's own verbs
// still verify against the type `create` returns.

export type Method =
  | "GET"
  | "POST"
  | "PUT"
  | "PATCH"
  | "DELETE"
  | "get"
  | "post"
  | "put"
  | "patch"
  | "delete";

export interface RequestConfig<D = unknown> {
  url?: string;
  method?: Method;
  data?: D;
  headers?: Record<string, string>;
  timeout?: number;
}

export interface CreateOptions {
  baseUrl?: string;
  headers?: Record<string, string>;
  timeout?: number;
}

export interface HttpResponse<T> {
  data: T;
  status: number;
}

export interface HttpInstance {
  <T = unknown, D = unknown>(config: RequestConfig<D>): Promise<HttpResponse<T>>;
  request<T = unknown, D = unknown>(config: RequestConfig<D>): Promise<HttpResponse<T>>;
  get<T = unknown>(url: string, config?: RequestConfig): Promise<HttpResponse<T>>;
  post<T = unknown, D = unknown>(
    url: string,
    data?: D,
    config?: RequestConfig<D>,
  ): Promise<HttpResponse<T>>;
}

export interface HttpStatic extends HttpInstance {
  create(options?: CreateOptions): HttpInstance;
}

declare const http: HttpStatic;
export default http;
