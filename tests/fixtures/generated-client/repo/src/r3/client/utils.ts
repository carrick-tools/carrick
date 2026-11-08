import type { Config, RequestOptions } from './types';

const PATH_PARAM = /\{[^{}]+\}/g;

export const serializePath = ({ path, url: _url }: { path: Record<string, string | number>; url: string }) => {
  let url = _url;
  const matches = _url.match(PATH_PARAM);
  if (matches) {
    for (const match of matches) {
      const name = match.substring(1, match.length - 1);
      url = url.replace(match, encodeURIComponent(String(path[name])));
    }
  }
  return url;
};

export const getUrl = ({ baseUrl, path, query, url: _url }: { baseUrl?: string; path?: Record<string, string | number>; query?: Record<string, string>; url: string }) => {
  const pathUrl = _url.startsWith('/') ? _url : `/${_url}`;
  let url = (baseUrl ?? '') + pathUrl;
  if (path) {
    url = serializePath({ path, url });
  }
  const search = query ? new URLSearchParams(query).toString() : '';
  if (search) {
    url += `?${search}`;
  }
  return url;
};

export const buildUrl = (options: RequestOptions & Config) =>
  getUrl({ baseUrl: options.baseUrl, path: options.path, query: options.query, url: options.url });

export const createConfig = (override: Config = {}): Config => ({ headers: {}, ...override });

export const mergeConfigs = (a: Config, b: Config): Config => ({ ...a, ...b, headers: { ...a.headers, ...b.headers } });
