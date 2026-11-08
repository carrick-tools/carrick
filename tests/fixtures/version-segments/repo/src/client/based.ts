import { http } from '@fixture/http';

declare function host(): string;

type O = { version?: 'v1' | 'v2' };

// A base the code does not state, then the defaulted key.
const request = (endpoint: string, options?: O) => {
  const { version = 'v1' } = options || {};
  return http.post(`${host()}/${version}${endpoint}`, null);
};

export const fetchOne = (endpoint: string) => request(endpoint);
export const fetchTwo = (endpoint: string) => request(endpoint, { version: 'v2' });
