import type { Config, Interceptors, RequestOptions } from './types';

export const buildUrl = (options: RequestOptions) => {
  let url = (options.baseUrl ?? '') + options.url;
  for (const [name, value] of Object.entries(options.path ?? {})) {
    url = url.replace(`{${name}}`, encodeURIComponent(String(value)));
  }
  const search = new URLSearchParams(options.query ?? {}).toString();
  return search ? `${url}?${search}` : url;
};

export const mergeHeaders = (...sets: Array<Record<string, string> | undefined>) => {
  const headers = new Headers();
  for (const set of sets) {
    for (const [key, value] of Object.entries(set ?? {})) {
      headers.set(key, value);
    }
  }
  return headers;
};

export const setAuthParams = async (options: RequestOptions & { headers: Headers }) => {
  for (const scheme of options.security ?? []) {
    if (scheme.type === 'http') {
      options.headers.set('Authorization', 'Bearer token');
    }
  }
};

export const createInterceptors = (): Interceptors => ({ request: { fns: [] }, response: { fns: [] } });

export const getParseAs = (contentType: string | null) =>
  contentType?.includes('json') ? 'json' : 'text';

export const createConfig = (override: Config = {}): Config => ({ headers: {}, parseAs: 'auto', ...override });

export const mergeConfigs = (a: Config, b: Config): Config => ({ ...a, ...b, headers: { ...a.headers, ...b.headers } });
