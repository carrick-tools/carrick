import { http } from '@fixture/http';

type O = { version?: 'v1' | 'v2'; body?: unknown };

const request = (endpoint: string, options?: O) => {
  const { version = 'v1', body } = options || {};
  return http.post(`/${version}${endpoint}`, { body });
};

// The key written before a spread of the caller's options: the caller may
// set it again.
export const sendV2 = (endpoint: string, options: O) => request(endpoint, { version: 'v2', ...options });
