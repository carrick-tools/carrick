import { http } from '@fixture/http';

type O = { version?: string };

export const call = (endpoint: string, options?: O) => {
  const { version = 'v1' } = options || {};
  return http.post(`/${version}${endpoint}`, { body: null });
};
