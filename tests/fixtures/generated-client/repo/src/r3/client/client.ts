import type { Client, Config, RequestOptions } from './types';
import { buildUrl, createConfig, mergeConfigs } from './utils';

type Interceptor = (request: Request, options: unknown) => Promise<Request>;

export const createClient = (config: Config = {}): Client => {
  let _config = mergeConfigs(createConfig(), config);

  const getConfig = (): Config => ({ ..._config });

  const setConfig = (next: Config): Config => {
    _config = mergeConfigs(_config, next);
    return getConfig();
  };

  const interceptors: Interceptor[] = [];

  const beforeRequest = async (options: RequestOptions) => {
    const opts = {
      ..._config,
      ...options,
      fetch: options.fetch ?? _config.fetch ?? globalThis.fetch,
    };
    if (opts.requestValidator) {
      await opts.requestValidator(opts);
    }
    const url = buildUrl(opts);
    return { opts, url };
  };

  const request: Client['request'] = async (options) => {
    const { opts, url } = await beforeRequest(options);
    let request = new Request(url, { method: opts.method, body: JSON.stringify(opts.body) });
    for (const fn of interceptors) {
      request = await fn(request, opts);
    }
    const _fetch = opts.fetch!;
    const response = await _fetch(request);
    return { data: await response.json(), response };
  };

  const makeMethodFn = (method: string) => (options: Omit<RequestOptions, 'method'>) =>
    request({ ...options, method });

  return {
    delete: makeMethodFn('DELETE'),
    get: makeMethodFn('GET'),
    getConfig,
    post: makeMethodFn('POST'),
    put: makeMethodFn('PUT'),
    request,
    setConfig,
  } as Client;
};
