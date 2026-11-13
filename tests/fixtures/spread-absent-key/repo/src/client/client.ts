import type { HttpMethod } from '../core/types';
import type { Client, Config, RequestOptions } from './types';
import { buildUrl, createConfig, createInterceptors, getParseAs, mergeConfigs, mergeHeaders, setAuthParams } from './utils';

export const createClient = (config: Config = {}): Client => {
  let _config = mergeConfigs(createConfig(), config);

  const getConfig = (): Config => ({ ..._config });

  const setConfig = (next: Config): Config => {
    _config = mergeConfigs(_config, next);
    return getConfig();
  };

  const interceptors = createInterceptors();

  const beforeRequest = async (options: RequestOptions) => {
    const opts = {
      ..._config,
      ...options,
      fetch: options.fetch ?? _config.fetch ?? globalThis.fetch,
      headers: mergeHeaders(_config.headers, options.headers),
      serializedBody: undefined as string | undefined,
    };
    if (opts.security) {
      await setAuthParams({ ...opts, security: opts.security });
    }
    if (opts.requestValidator) {
      await opts.requestValidator(opts);
    }
    if (opts.body !== undefined && opts.bodySerializer) {
      opts.serializedBody = opts.bodySerializer(opts.body);
    }
    if (opts.body === undefined || opts.serializedBody === '') {
      opts.headers.delete('Content-Type');
    }
    const url = buildUrl(opts);
    return { opts, url };
  };

  const request: Client['request'] = async (options) => {
    const { opts, url } = await beforeRequest(options);
    const requestInit = {
      redirect: 'follow' as const,
      ...opts,
      body: opts.serializedBody,
    };
    let request = new Request(url, requestInit);
    for (const fn of interceptors.request.fns) {
      if (fn) {
        request = await fn(request, opts);
      }
    }
    const _fetch = opts.fetch!;
    let response = await _fetch(request);
    for (const fn of interceptors.response.fns) {
      if (fn) {
        response = await fn(response, request, opts);
      }
    }
    if (response.ok) {
      const parseAs = (opts.parseAs === 'auto' ? getParseAs(response.headers.get('Content-Type')) : opts.parseAs) ?? 'json';
      let data: unknown;
      switch (parseAs) {
        case 'blob':
        case 'json':
        case 'text':
          data = await response[parseAs]();
          break;
      }
      return { data };
    }
    const textError = await response.text();
    let error: unknown = textError;
    try {
      error = JSON.parse(textError);
    } catch {
      // keep the text
    }
    if (opts.throwOnError) {
      throw error;
    }
    return { error };
  };

  const makeMethodFn = (method: Uppercase<HttpMethod>) => (options: Omit<RequestOptions, 'method'>) =>
    request({ ...options, method });

  return {
    delete: makeMethodFn('DELETE'),
    get: makeMethodFn('GET'),
    getConfig,
    interceptors,
    patch: makeMethodFn('PATCH'),
    post: makeMethodFn('POST'),
    put: makeMethodFn('PUT'),
    request,
    setConfig,
  } as Client;
};
