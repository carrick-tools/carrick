import { http } from '@fixture/http';

type Options = { method?: 'GET' | 'POST'; version?: 'v1' | 'v2'; body?: unknown };
type Extra = { body?: unknown; version?: 'v1' | 'v2' };

// Options destructured in the body, each key with a default.
const request = (endpoint: string, options?: Options) => {
  const { method = 'GET', version = 'v1', body } = options || {};
  return http.post(`/${version}${endpoint}`, { method, body });
};

export const get = (endpoint: string) => request(endpoint, { method: 'GET' });
export const getV2 = (endpoint: string) => request(endpoint, { version: 'v2', method: 'GET' });
export const post = (endpoint: string, options: Extra) => request(endpoint, { method: 'POST', ...options });
export const postV2 = (endpoint: string, options: Extra) =>
  request(endpoint, { version: 'v2', method: 'POST', ...options });
