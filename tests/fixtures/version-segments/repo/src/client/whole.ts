import { http } from '@fixture/http';

type O = { version?: 'v1' | 'v2' };

// The defaulted key is a whole path segment.
export const items = (options?: O) => {
  const { version = 'v1' } = options || {};
  return http.post(`/${version}/items`, null);
};
