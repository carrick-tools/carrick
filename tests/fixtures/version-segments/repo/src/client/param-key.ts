import { http } from '@fixture/http';

type O = { version?: 'v1' | 'v2' };

// A destructured parameter whose key has a default.
export function keyed(endpoint: string, { version = 'v1' }: O = {}) {
  return http.post(`/${version}${endpoint}`, { body: null });
}
