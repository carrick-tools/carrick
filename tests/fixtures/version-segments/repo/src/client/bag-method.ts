import { http } from '@fixture/http';

type O = { method?: 'PUT' | 'PATCH'; body?: unknown };

// The options bag the call sends states the verb: the client defaults it.
const send = (endpoint: string, options?: O) => {
  const { method = 'PUT', body } = options || {};
  return http.post(`/bm${endpoint}`, { method, body });
};

export const put = (endpoint: string) => send(endpoint, { body: 1 });
export const patch = (endpoint: string) => send(endpoint, { method: 'PATCH', body: 1 });
